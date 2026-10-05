//! The cpu-qos suite: how much slower a latency class cell gets while best effort cells keep every
//! core busy, next to the same load with no classes at all.
//!
//! A probe cell runs a fixed piece of Python work again and again with a short sleep in between,
//! the way an agent's step computes and then waits, and times every step inside the cell so the
//! gRPC hop is not in the number. Hog cells each run a few processes that spin until a deadline
//! and count how much they got done. Every round measures three ways, one after the other, so slow
//! drift on the machine falls on all three alike:
//!
//! - alone: the probe with no hogs;
//! - no QoS: the probe and the hogs all in the standard class;
//! - QoS: the probe in the latency class and the hogs in the best effort class.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use hive_sdk::{Cell, Error, Qos, Reason, Resources};

use crate::node::{Sample, Target, ms};
use crate::stats::Summary;

/// The probe: `n` steps of `w` loop turns each, `p` ms of sleep after each, step times in µs on
/// one line.
const PROBE: &str = r#"
import sys, time
n, w, p = int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3]) / 1000
def work():
    s = 0
    for i in range(w):
        s += i * i
    return s
for _ in range(3):
    work()
out = []
for _ in range(n):
    t = time.perf_counter()
    work()
    out.append(time.perf_counter() - t)
    time.sleep(p)
print(" ".join(str(round(x * 1e6)) for x in out))
"#;

/// A hog: `l` processes that spin for `s` seconds, then the batches of work they finished in all.
const HOG: &str = r#"
import os, sys, time
l, s = int(sys.argv[1]), float(sys.argv[2])
end = time.monotonic() + s
r, w = os.pipe()
for _ in range(l):
    if os.fork() == 0:
        n = 0
        while time.monotonic() < end:
            for _ in range(10000):
                pass
            n += 1
        os.write(w, f"{n}\n".encode())
        os._exit(0)
os.close(w)
for _ in range(l):
    os.wait()
print(sum(int(x) for x in os.fdopen(r).read().split()))
"#;

/// What a cpu-qos run does.
#[derive(Clone, Debug)]
pub struct QosOpts {
    /// Hog cells per contended mode.
    pub hogs: u32,
    /// Spinning processes in each hog cell, which is also the cores it asks for.
    pub loops: u32,
    /// Probe steps per mode per round.
    pub steps: u32,
    /// Loop turns in one probe step.
    pub work: u32,
    /// Sleep after each probe step.
    pub pause: Duration,
    /// How long the hogs spin. The probe has to be done well inside it.
    pub hog_for: Duration,
    /// Rounds of all three modes.
    pub rounds: u32,
}

/// One way of running the probe, over every round.
#[derive(Clone, Debug)]
pub struct Mode {
    /// alone, no QoS or QoS.
    pub name: &'static str,
    /// Every step's time, as the probe measured it.
    pub steps: Vec<Duration>,
    /// Batches of work the hogs finished, over every round.
    pub hog_work: u64,
    /// Rounds where the probe was still running when the hogs stopped.
    pub overran: u32,
}

impl Mode {
    fn new(name: &'static str) -> Self {
        Self { name, steps: Vec::new(), hog_work: 0, overran: 0 }
    }

    /// The steps as raw samples, each at the sum of the steps before it.
    #[must_use]
    pub fn samples(&self) -> Vec<Sample> {
        let mut at = Duration::ZERO;
        let mut out = Vec::with_capacity(self.steps.len());
        for &took in &self.steps {
            out.push(Sample { at, took, error: None });
            at += took;
        }
        out
    }

    fn mean(&self) -> Duration {
        let n = u32::try_from(self.steps.len()).unwrap_or(u32::MAX).max(1);
        self.steps.iter().sum::<Duration>() / n
    }
}

struct Cells {
    probe_std: Cell,
    probe_lat: Cell,
    hogs_std: Vec<Cell>,
    hogs_be: Vec<Cell>,
}

/// Runs every round and returns alone, no QoS and QoS, in that order.
///
/// # Errors
///
/// A cell could not be made, or the probe or a hog did not run or printed something else.
pub async fn run(t: &Target, o: &QosOpts) -> Result<[Mode; 3], Error> {
    let cells = make(t, o).await?;
    let mut modes = [Mode::new("alone"), Mode::new("no QoS"), Mode::new("QoS")];
    for round in 1..=o.rounds {
        let alone = probe(&cells.probe_lat, o).await?;
        let (std, std_work, std_over) = contended(&cells.probe_std, &cells.hogs_std, o).await?;
        let (lat, be_work, be_over) = contended(&cells.probe_lat, &cells.hogs_be, o).await?;
        for (m, (steps, work, over)) in modes.iter_mut().zip([
            (alone, 0, false),
            (std, std_work, std_over),
            (lat, be_work, be_over),
        ]) {
            eprintln!(
                "round {round} {}: p50 {} ms, p99 {} ms{}",
                m.name,
                ms(Summary::of(&steps).map_or(Duration::ZERO, |s| s.p50)),
                ms(Summary::of(&steps).map_or(Duration::ZERO, |s| s.p99)),
                if over { ", ran past the hogs" } else { "" }
            );
            m.steps.extend(steps);
            m.hog_work += work;
            m.overran += u32::from(over);
        }
    }
    Ok(modes)
}

async fn make(t: &Target, o: &QosOpts) -> Result<Cells, Error> {
    let one = |step: &str, qos: Qos, cores: u32| {
        let mut spec = t.spec(step);
        spec.qos = qos;
        spec.resources = Resources { vcpu_milli: cores * 1000, mem_mib: 256, ..Resources::DEFAULT };
        spec
    };
    let many = |spec, n| async move {
        t.client.create_many(&spec, n, None).await?.into_iter().collect::<Result<Vec<_>, _>>()
    };
    let probe_std = t.client.create(&one("probe-standard", Qos::Standard, 1)).await?;
    let probe_lat = t.client.create(&one("probe-latency", Qos::Latency, 1)).await?;
    let hogs_std = many(one("hog-standard", Qos::Standard, o.loops), o.hogs).await?;
    let hogs_be = many(one("hog-best-effort", Qos::BestEffort, o.loops), o.hogs).await?;
    Ok(Cells { probe_std, probe_lat, hogs_std, hogs_be })
}

/// Starts the hogs, gives them a second to get going, runs the probe, and waits for the hogs.
async fn contended(
    probe_cell: &Cell,
    hogs: &[Cell],
    o: &QosOpts,
) -> Result<(Vec<Duration>, u64, bool), Error> {
    let started = Instant::now();
    let argv = |c: &Cell| {
        let c = c.clone();
        let args = vec![
            "python3".to_string(),
            "-c".to_string(),
            HOG.to_string(),
            o.loops.to_string(),
            o.hog_for.as_secs_f64().to_string(),
        ];
        tokio::spawn(async move { c.run(args).await })
    };
    let running: Vec<_> = hogs.iter().map(argv).collect();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let steps = probe(probe_cell, o).await?;
    let overran = started.elapsed() > o.hog_for;
    let mut work = 0;
    for h in running {
        let r = h.await.map_err(|e| Error::new(Reason::Internal, e.to_string()))??;
        let out = String::from_utf8_lossy(&r.stdout);
        work += out.trim().parse::<u64>().map_err(|_| {
            Error::new(
                Reason::Internal,
                format!(
                    "a hog printed {:?} and {:?}",
                    out.trim(),
                    String::from_utf8_lossy(&r.stderr)
                ),
            )
        })?;
    }
    Ok((steps, work, overran))
}

async fn probe(cell: &Cell, o: &QosOpts) -> Result<Vec<Duration>, Error> {
    let args = vec![
        "python3".to_string(),
        "-c".to_string(),
        PROBE.to_string(),
        o.steps.to_string(),
        o.work.to_string(),
        o.pause.as_millis().to_string(),
    ];
    let r = cell.run(args).await?;
    let out = String::from_utf8_lossy(&r.stdout);
    let steps: Vec<Duration> = out
        .split_whitespace()
        .filter_map(|us| us.parse().ok())
        .map(Duration::from_micros)
        .collect();
    if steps.len() != o.steps as usize {
        return Err(Error::new(
            Reason::Internal,
            format!(
                "the probe printed {} steps: {}",
                steps.len(),
                String::from_utf8_lossy(&r.stderr)
            ),
        ));
    }
    Ok(steps)
}

/// The markdown table: each mode's step times, how far its p50 and p99 are past alone, and how
/// much work the hogs got done next to no QoS.
#[must_use]
pub fn table(modes: &[Mode; 3]) -> String {
    let mut s = String::from(
        "| mode | steps | p50 ms | p90 ms | p99 ms | mean ms | p50 vs alone | p99 vs alone | hog work vs no QoS |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    let alone = Summary::of(&modes[0].steps);
    let pct = |a: Duration, b: Duration| {
        format!("{:+.1}%", (a.as_secs_f64() / b.as_secs_f64() - 1.0) * 100.0)
    };
    for m in modes {
        let Some(sum) = Summary::of(&m.steps) else { continue };
        let (vs50, vs99) = match &alone {
            Some(a) if m.name != "alone" => (pct(sum.p50, a.p50), pct(sum.p99, a.p99)),
            _ => ("".into(), "".into()),
        };
        let work = match m.name {
            "alone" => String::new(),
            _ => format!("{:.2}", m.hog_work as f64 / modes[1].hog_work.max(1) as f64),
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {vs50} | {vs99} | {work} |",
            m.name,
            sum.count,
            ms(sum.p50),
            ms(sum.p90),
            ms(sum.p99),
            ms(m.mean()),
        );
    }
    s
}
