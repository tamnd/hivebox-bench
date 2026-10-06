//! The density suite: how many cells a node holds while an exec in any of them still answers fast.
//!
//! Cells are added in steps. In every step each cell runs a loader that keeps it busy for a share
//! of one core, drawn once per cell from the DSec CPU distribution, where 90% of cells use 5% of a
//! core or less: 90% of the cells get a share from 1% to 5% and the rest from 5% to 50%. The
//! loader spins for its share of every 100 ms and sleeps the rest. Once the loaders are going,
//! `true` is run in cells all over the step, a few at a time, and each run is timed from the
//! caller. The answer is the largest step whose p99 stays at 25 ms or less. Each cell asks for one
//! core, so no loader runs into its cell's CPU limit.
//!
//! A step is not started while the host has less memory available than the floor, so a full
//! machine ends the run instead of the kernel's OOM killer.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hive_sdk::{Cell, Error, Resources};

use crate::cluster::Rng;
use crate::node::{Sample, Target, ms};
use crate::stats::Summary;

/// The loader: a share of one core, as a fraction, for `argv[2]` seconds.
const LOADER: &str = r"
import sys, time
d, s = float(sys.argv[1]), float(sys.argv[2])
end = time.monotonic() + s
while True:
    t = time.monotonic()
    if t >= end:
        break
    while time.monotonic() - t < d * 0.1:
        pass
    time.sleep(max(0.0, 0.1 - (time.monotonic() - t)))
";

/// The p99 an exec has to stay under for a step to count.
pub const TARGET: Duration = Duration::from_millis(25);

/// What a density run does.
#[derive(Clone, Debug)]
pub struct DensityOpts {
    /// The cell counts of the steps, smallest first.
    pub counts: Vec<u32>,
    /// Timed `true` runs per step.
    pub probes: u32,
    /// How many of them are in flight at once.
    pub parallel: u32,
    /// How long the loaders run before the probes start.
    pub warm: Duration,
    /// The memory of each cell, in MiB.
    pub cell_mib: u32,
    /// The least memory the host has to have available, in MiB, for the next step to start.
    pub floor_mib: u64,
    /// The comb's cgroup root, whose memory is read at each step, if given.
    pub cgroup: Option<PathBuf>,
    /// The seed for the CPU shares.
    pub seed: u64,
    /// What every share is multiplied by. Zero runs no loaders, which measures the node without
    /// the load.
    pub load: f64,
}

/// One step: the cells alive and how their execs went.
#[derive(Clone, Debug, Default)]
pub struct Step {
    /// Cells alive during the step.
    pub cells: u32,
    /// Creates that failed in this step, with the first error.
    pub refused: (u32, Option<String>),
    /// The sum of the cells' CPU shares, in cores.
    pub cores: f64,
    /// Every timed `true`.
    pub execs: Vec<Sample>,
    /// The comb's `memory.current` once the probes were done, in bytes.
    pub mem: Option<u64>,
    /// `MemAvailable` of the host once the probes were done, in bytes.
    pub available: Option<u64>,
    /// Loaders that ended before the probes did, or failed.
    pub short: u32,
}

/// Whether a run ended before its last step, and why.
#[derive(Clone, Debug, Default)]
pub struct Results {
    /// The steps that ran.
    pub steps: Vec<Step>,
    /// Why the run stopped early, if it did.
    pub stopped: Option<String>,
}

/// The share of a core of one cell: 1% to 5% for 90% of cells and 5% to 50% for the rest.
fn share(rng: &mut Rng) -> f64 {
    if rng.unit() < 0.9 { 0.01 + rng.unit() * 0.04 } else { 0.05 + rng.unit() * 0.45 }
}

/// Runs the steps.
///
/// # Errors
///
/// A step had no cells at all, or the SDK failed outside a create.
///
/// # Panics
///
/// A loader or probe task panicked, which is a bug in the SDK.
pub async fn run(t: &Target, o: &DensityOpts) -> Result<Results, Error> {
    let mut spec = t.spec("density");
    spec.resources = Resources { mem_mib: o.cell_mib, ..Resources::DEFAULT };
    let mut rng = Rng::new(o.seed);
    let mut cells: Vec<(Cell, f64)> = Vec::new();
    let mut out = Results::default();
    for &count in &o.counts {
        if let Some(a) = available()
            && a < o.floor_mib << 20
        {
            out.stopped = Some(format!(
                "the host had {} MiB available, under the floor of {} MiB",
                a >> 20,
                o.floor_mib
            ));
            break;
        }
        let mut step = Step::default();
        while cells.len() < count as usize {
            let n = (count - u32::try_from(cells.len()).unwrap_or(0)).min(16);
            let mut made = 0;
            for c in t.client.create_many(&spec, n, None).await? {
                match c {
                    Ok(c) => {
                        cells.push((c, share(&mut rng) * o.load));
                        made += 1;
                    }
                    Err(e) => {
                        step.refused.0 += 1;
                        step.refused.1.get_or_insert_with(|| e.to_string());
                    }
                }
            }
            if made == 0 {
                break;
            }
        }
        if cells.is_empty() {
            out.stopped =
                Some(format!("no cell could be made: {}", step.refused.1.unwrap_or_default()));
            break;
        }
        step.cells = u32::try_from(cells.len()).unwrap_or(u32::MAX);
        step.cores = cells.iter().map(|(_, s)| s).sum();
        eprintln!(
            "{} cells, {:.2} cores of load, {} creates refused",
            step.cells, step.cores, step.refused.0
        );
        // The loaders run long enough for the probes, at a few ms each and a second at worst.
        let hold = o.warm + Duration::from_secs(u64::from(o.probes / o.parallel.max(1)) + 30);
        let loaders: Vec<_> = cells
            .iter()
            .filter(|(_, s)| *s > 0.0)
            .map(|(c, s)| {
                let c = c.clone();
                let argv = vec![
                    "python3".to_string(),
                    "-c".to_string(),
                    LOADER.to_string(),
                    format!("{s:.4}"),
                    hold.as_secs_f64().to_string(),
                ];
                tokio::spawn(async move { c.run(argv).await.map(|r| r.exit_code) })
            })
            .collect();
        tokio::time::sleep(o.warm).await;
        let started = Instant::now();
        step.execs = probe(&cells, o, started).await;
        let probed = started.elapsed();
        step.mem = o.cgroup.as_ref().and_then(|g| {
            std::fs::read_to_string(g.join("memory.current")).ok()?.trim().parse().ok()
        });
        step.available = available();
        for l in loaders {
            let ok = matches!(l.await.expect("a loader task panicked"), Ok(0));
            if !ok || o.warm + probed > hold {
                step.short += 1;
            }
        }
        let full = step.cells < count;
        out.steps.push(step);
        if full {
            out.stopped = Some(format!("the node took no more than {} cells", cells.len()));
            break;
        }
    }
    Ok(out)
}

/// The timed `true` runs, `o.parallel` at a time, spread over all the cells.
async fn probe(cells: &[(Cell, f64)], o: &DensityOpts, started: Instant) -> Vec<Sample> {
    let all: Arc<Vec<Cell>> = Arc::new(cells.iter().map(|(c, _)| c.clone()).collect());
    let per = o.probes / o.parallel.max(1);
    let workers: Vec<_> = (0..o.parallel.max(1))
        .map(|w| {
            let all = all.clone();
            tokio::spawn(async move {
                let mut out = Vec::new();
                for i in 0..per {
                    // A stride that is prime to most counts walks every cell before it repeats.
                    let k = (w * per + i) as usize * 7919 % all.len();
                    let at = Instant::now();
                    let r = all[k].run(vec!["true".to_string()]).await;
                    let took = at.elapsed();
                    let error = match r {
                        Ok(r) if r.exit_code == 0 => None,
                        Ok(r) => Some(format!("exit {}", r.exit_code)),
                        Err(e) => Some(e.to_string()),
                    };
                    out.push(Sample { at: at - started, took, error });
                }
                out
            })
        })
        .collect();
    let mut out = Vec::new();
    for w in workers {
        out.extend(w.await.expect("a probe task panicked"));
    }
    out.sort_by_key(|s| s.at);
    out
}

/// `MemAvailable` of the host, in bytes.
fn available() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 = info
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kib << 10)
}

/// The largest step whose exec p99 is at [`TARGET`] or less, with no failed execs.
#[must_use]
pub fn most(r: &Results) -> Option<u32> {
    r.steps
        .iter()
        .filter(|s| s.execs.iter().all(|e| e.error.is_none()))
        .filter(|s| Summary::of(&ok(s)).is_some_and(|x| x.p99 <= TARGET))
        .map(|s| s.cells)
        .max()
}

fn ok(s: &Step) -> Vec<Duration> {
    s.execs.iter().filter(|e| e.error.is_none()).map(|e| e.took).collect()
}

/// The markdown table, one row per step.
#[must_use]
pub fn table(r: &Results) -> String {
    let mut s = String::from(
        "| cells | load cores | refused | execs | failed | p50 ms | p90 ms | p99 ms | max ms | comb MiB | host available MiB | loaders short |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    let mib = |b: Option<u64>| b.map_or("-".into(), |b| (b >> 20).to_string());
    for st in &r.steps {
        let ok = ok(st);
        let failed = st.execs.len() - ok.len();
        let (p50, p90, p99, max) = Summary::of(&ok).map_or_else(
            || ("-".into(), "-".into(), "-".into(), "-".into()),
            |x| (ms(x.p50), ms(x.p90), ms(x.p99), ms(x.max)),
        );
        let _ = writeln!(
            s,
            "| {} | {:.2} | {} | {} | {failed} | {p50} | {p90} | {p99} | {max} | {} | {} | {} |",
            st.cells,
            st.cores,
            st.refused.0,
            st.execs.len(),
            mib(st.mem),
            mib(st.available),
            st.short,
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_in_ten_cells_use_five_percent_or_less() {
        let mut rng = Rng::new(7);
        let shares: Vec<f64> = (0..10_000).map(|_| share(&mut rng)).collect();
        let small = shares.iter().filter(|&&s| s <= 0.05).count();
        assert!((8_800..=9_200).contains(&small), "{small}");
        assert!(shares.iter().all(|&s| (0.01..=0.5).contains(&s)));
    }

    #[test]
    fn the_most_is_the_largest_step_under_the_target() {
        let step = |cells, took| Step {
            cells,
            execs: vec![Sample {
                at: Duration::ZERO,
                took: Duration::from_millis(took),
                error: None,
            }],
            ..Step::default()
        };
        let r = Results { steps: vec![step(10, 5), step(20, 24), step(40, 80)], stopped: None };
        assert_eq!(most(&r), Some(20));
        let t = table(&r);
        assert!(
            t.contains("| 20 | 0.00 | 0 | 1 | 0 | 24.0 | 24.0 | 24.0 | 24.0 | - | - | 0 |"),
            "{t}"
        );
    }
}
