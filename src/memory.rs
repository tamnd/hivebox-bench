//! The memory suite: how much memory a node full of agent cells holds over time, at its peak and
//! on average, which is what decides how many cells fit.
//!
//! Each agent cell runs a step and then thinks for a while, the way an agent waits on its model.
//! A step imports a handful of standard library modules, reads every `.py` file of the standard
//! library, and writes a scratch file once and reads it back every step after, so the cell holds
//! page cache it does not need while it thinks. The agents start spread over one think time so
//! their steps do not all land at once. All the while the suite reads `memory.current` and
//! `memory.stat` of the comb's cgroup root every second.
//!
//! The suite measures one comb as it is configured. To compare, run it against a comb with
//! `[density] trim_idle = "0s"` and again with trimming on, and compare the two tables.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hive_sdk::{Cell, Error, Reason, Resources};

use crate::node::{Sample, Target, ms};
use crate::stats::Summary;

/// One agent step: the imports, the standard library read, and the scratch file of `argv[1]`
/// MiB, which is written the first time and read every time.
const STEP: &str = r#"
import sys, os, sysconfig, hashlib
import json, sqlite3, email.parser, http.client, unittest, xml.dom.minidom, decimal, asyncio
mib = int(sys.argv[1])
n = 0
for root, _, files in os.walk(sysconfig.get_paths()["stdlib"]):
    for f in files:
        if f.endswith(".py"):
            with open(os.path.join(root, f), "rb") as fh:
                n += len(fh.read())
p = "/root/scratch.bin"
if not os.path.exists(p):
    with open(p, "wb") as fh:
        fh.write(os.urandom(mib << 20))
h = hashlib.sha256()
with open(p, "rb") as fh:
    for b in iter(lambda: fh.read(1 << 20), b""):
        h.update(b)
print(n, h.hexdigest()[:8])
"#;

/// What a memory run does.
#[derive(Clone, Debug)]
pub struct MemOpts {
    /// Agent cells.
    pub agents: u32,
    /// How long an agent waits after each step.
    pub think: Duration,
    /// How long the agents run, from when the last one has started.
    pub length: Duration,
    /// The MiB of each agent's scratch file.
    pub scratch: u32,
    /// The comb's cgroup root, whose memory is read.
    pub cgroup: PathBuf,
}

/// One reading of the cgroup root.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reading {
    /// When, from the start of the measured window.
    pub at: Duration,
    /// `memory.current`, in bytes.
    pub current: u64,
    /// `file` in `memory.stat`, page cache.
    pub file: u64,
    /// `anon` in `memory.stat`.
    pub anon: u64,
}

/// What a memory run measured.
#[derive(Clone, Debug, Default)]
pub struct Results {
    /// Every reading in the measured window.
    pub readings: Vec<Reading>,
    /// Every step that finished, as the caller timed it.
    pub steps: Vec<Sample>,
    /// Steps that failed or exited non zero, with the first one.
    pub failed: (u32, Option<String>),
}

/// Makes the agents, runs them for the measured window, and returns the readings and steps.
///
/// # Errors
///
/// The agents could not all be made, or the cgroup root cannot be read.
///
/// # Panics
///
/// An agent task panicked, which is a bug in the SDK.
pub async fn run(t: &Target, o: &MemOpts) -> Result<Results, Error> {
    read(o).map_err(|e| Error::new(Reason::Internal, format!("{}: {e}", o.cgroup.display())))?;
    let mut spec = t.spec("agent");
    spec.resources = Resources { mem_mib: 512, ..Resources::DEFAULT };
    let made = Instant::now();
    let mut cells = Vec::new();
    while cells.len() < o.agents as usize {
        let n = (o.agents - u32::try_from(cells.len()).unwrap_or(0)).min(8);
        for c in t.client.create_many(&spec, n, None).await? {
            cells.push(c?);
        }
    }
    eprintln!("{} agents made in {} ms", cells.len(), ms(made.elapsed()));
    let start = Instant::now();
    let warm = o.think;
    let end = start + warm + o.length;
    let mut tasks = Vec::new();
    for (i, cell) in cells.into_iter().enumerate() {
        let offset = o.think * u32::try_from(i).unwrap_or(0) / o.agents.max(1);
        let scratch = o.scratch;
        let think = o.think;
        tasks.push(tokio::spawn(async move {
            tokio::time::sleep(offset).await;
            agent(&cell, scratch, think, start, end).await
        }));
    }
    let mut readings = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    while Instant::now() < end {
        tick.tick().await;
        let at = start.elapsed();
        if at < warm {
            continue;
        }
        if let Ok(mut r) = read(o) {
            r.at = at - warm;
            readings.push(r);
        }
    }
    let mut out = Results { readings, ..Results::default() };
    for task in tasks {
        let (steps, failed, first) = task.await.expect("an agent task panicked");
        out.steps.extend(steps);
        out.failed.0 += failed;
        if out.failed.1.is_none() {
            out.failed.1 = first;
        }
    }
    out.steps.sort_by_key(|s| s.at);
    Ok(out)
}

/// One agent: a step, then `think`, until `end`. Steps before the measured window are not kept.
async fn agent(
    cell: &Cell,
    scratch: u32,
    think: Duration,
    start: Instant,
    end: Instant,
) -> (Vec<Sample>, u32, Option<String>) {
    let (mut steps, mut failed, mut first) = (Vec::new(), 0, None);
    let argv = vec!["python3".to_string(), "-c".to_string(), STEP.to_string(), scratch.to_string()];
    while Instant::now() < end {
        let at = Instant::now();
        let r = cell.run(argv.clone()).await;
        let took = at.elapsed();
        let error = match r {
            Ok(r) if r.exit_code == 0 => None,
            Ok(r) => Some(format!("exit {}: {}", r.exit_code, String::from_utf8_lossy(&r.stderr))),
            Err(e) => Some(e.to_string()),
        };
        if let Some(e) = &error {
            failed += 1;
            first.get_or_insert_with(|| e.clone());
        }
        if at >= start + think {
            steps.push(Sample { at: at - start - think, took, error });
        }
        tokio::time::sleep(think).await;
    }
    (steps, failed, first)
}

fn read(o: &MemOpts) -> std::io::Result<Reading> {
    let current = std::fs::read_to_string(o.cgroup.join("memory.current"))?;
    let stat = std::fs::read_to_string(o.cgroup.join("memory.stat"))?;
    let field = |name: &str| {
        stat.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.trim().parse().ok())
            .unwrap_or(0)
    };
    Ok(Reading {
        at: Duration::ZERO,
        current: current.trim().parse().unwrap_or(0),
        file: field("file"),
        anon: field("anon"),
    })
}

fn mib(bytes: f64) -> String {
    format!("{:.0}", bytes / f64::from(1 << 20))
}

/// The markdown table: peak and mean memory over the window, and the step times.
#[must_use]
pub fn table(r: &Results) -> String {
    let n = r.readings.len().max(1) as f64;
    let mean = |f: fn(&Reading) -> u64| r.readings.iter().map(|x| f(x) as f64).sum::<f64>() / n;
    let peak = r.readings.iter().map(|x| x.current).max().unwrap_or(0);
    let ok: Vec<Duration> = r.steps.iter().filter(|s| s.error.is_none()).map(|s| s.took).collect();
    let (p50, p99) = Summary::of(&ok).map_or(("-".into(), "-".into()), |s| (ms(s.p50), ms(s.p99)));
    let mut s = String::from(
        "| readings | peak MiB | mean MiB | mean file MiB | mean anon MiB | steps | failed | step p50 ms | step p99 ms |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    let _ = writeln!(
        s,
        "| {} | {} | {} | {} | {} | {} | {} | {p50} | {p99} |",
        r.readings.len(),
        mib(peak as f64),
        mib(mean(|x| x.current)),
        mib(mean(|x| x.file)),
        mib(mean(|x| x.anon)),
        ok.len(),
        r.failed.0,
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_peak_and_mean() {
        let m = 1 << 20;
        let at = Duration::ZERO;
        let r = Results {
            readings: vec![
                Reading { at, current: 100 * m, file: 60 * m, anon: 40 * m },
                Reading { at, current: 300 * m, file: 200 * m, anon: 100 * m },
            ],
            steps: vec![Sample { at, took: Duration::from_millis(900), error: None }],
            failed: (0, None),
        };
        let t = table(&r);
        assert!(t.contains("| 2 | 300 | 200 | 130 | 70 | 1 | 0 | 900.0 | 900.0 |"), "{t}");
    }
}
