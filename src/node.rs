//! The node suites that run against one comb: create-storm and exec.
//!
//! Both talk to the comb the way a user does, through `hive-sdk`, so a number here includes the
//! gRPC hop and the SDK and not only the work inside the node. Every cell a suite makes carries a
//! `bench` label with the run's id, and the suite stops them by that label when it is done, so a
//! run that fails half way leaves nothing behind that a second run could mistake for its own.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hive_sdk::{Backend, Cell, CellSpec, Client, Error, Selector, Source};

use crate::stats::Summary;

/// Where the cells come from and how they are labelled.
#[derive(Clone, Debug)]
pub struct Target {
    /// The client, already scoped to the project the run uses.
    pub client: Client,
    /// The image every cell starts from.
    pub image: String,
    /// The value of the `bench` label on every cell of this run.
    pub run: String,
}

impl Target {
    fn spec(&self, step: &str) -> CellSpec {
        let mut spec = CellSpec::new(Source::Image(self.image.clone()), Backend::Container);
        spec.labels = BTreeMap::from([
            ("bench".to_string(), self.run.clone()),
            ("step".to_string(), step.to_string()),
        ]);
        spec
    }

    /// Stops every cell of this run, and returns how many there were and how long it took.
    ///
    /// # Errors
    ///
    /// The bulk stop call itself failed.
    pub async fn stop_all(&self) -> Result<(u32, Duration), Error> {
        let t = Instant::now();
        let labels = BTreeMap::from([("bench".to_string(), self.run.clone())]);
        let r = self.client.stop(&Selector::Labels(labels)).await?;
        Ok((r.matched, t.elapsed()))
    }
}

/// One call a step made: when it started, from the start of the step, how long it took, and why
/// it failed if it did. These are what the raw results file holds.
#[derive(Clone, Debug)]
pub struct Sample {
    /// When the call started, from the start of its step.
    pub at: Duration,
    /// How long it took to answer.
    pub took: Duration,
    /// The failure, if it failed.
    pub error: Option<String>,
}

/// One step of a create storm: creates started at a fixed rate for a fixed time.
#[derive(Clone, Debug)]
pub struct StormStep {
    /// The rate the creates were started at, per second.
    pub rate: u32,
    /// How many were started.
    pub started: usize,
    /// How many failed, with the reason of the first failure.
    pub failed: usize,
    /// The first failure, if any.
    pub first_error: Option<String>,
    /// How many of the failures were the platform's and not the caller's.
    pub infra: usize,
    /// Wall time from the first start to the last answer.
    pub wall: Duration,
    /// The latency of every create that succeeded.
    pub latency: Option<Summary>,
    /// How long stopping the step's cells took, and how many there were.
    pub stopped: (u32, Duration),
    /// Every create, in the order they were started.
    pub samples: Vec<Sample>,
}

impl StormStep {
    /// Creates that succeeded per second of wall time, which falls below `rate` once the node
    /// cannot keep up.
    #[must_use]
    pub fn achieved(&self) -> f64 {
        (self.started - self.failed) as f64 / self.wall.as_secs_f64()
    }

    /// Whether this step is past what the node sustains: any failure, or p99 over `p99_limit`.
    #[must_use]
    pub fn broke(&self, p99_limit: Duration) -> bool {
        self.failed > 0 || self.latency.is_none_or(|s| s.p99 > p99_limit)
    }
}

/// Starts `rate` creates a second for `seconds`, open loop, so a slow create does not slow the
/// arrivals down the way a closed loop would hide it. Every cell is stopped at the end of the
/// step, before the next one starts.
///
/// # Errors
///
/// Stopping the cells failed, which leaves the node in a state the next step cannot trust.
///
/// # Panics
///
/// A create task panicked, which is a bug in the SDK.
pub async fn storm_step(t: &Target, rate: u32, seconds: u32) -> Result<StormStep, Error> {
    let spec = Arc::new(t.spec(&format!("storm-{rate}")));
    let total = (rate * seconds) as usize;
    let gap = Duration::from_secs(1) / rate.max(1);
    let start = tokio::time::Instant::now();
    let mut tasks = Vec::with_capacity(total);
    for i in 0..total {
        tokio::time::sleep_until(start + gap * i as u32).await;
        let (client, spec) = (t.client.clone(), spec.clone());
        let at = start.elapsed();
        tasks.push(tokio::spawn(async move {
            let t = Instant::now();
            let r = client.create(&spec).await;
            (at, t.elapsed(), r.map(|_| ()))
        }));
    }
    let (mut ok, mut failed, mut infra, mut first_error) = (Vec::new(), 0, 0, None);
    let mut samples = Vec::with_capacity(total);
    for task in tasks {
        let (at, took, r) = task.await.expect("a create task panicked");
        let error = match r {
            Ok(()) => {
                ok.push(took);
                None
            }
            Err(e) => {
                failed += 1;
                infra += usize::from(e.reason.is_infra());
                first_error.get_or_insert_with(|| e.to_string());
                Some(e.to_string())
            }
        };
        samples.push(Sample { at, took, error });
    }
    let wall = start.elapsed();
    let stopped = t.stop_all().await?;
    Ok(StormStep {
        rate,
        started: total,
        failed,
        first_error,
        infra,
        wall,
        latency: Summary::of(&ok),
        stopped,
        samples,
    })
}

/// Makes `n` cells, `batch` per call with two calls in flight, and keeps them. Returns the cells,
/// how long it took and the failures. Two calls keep the node busy without queueing more creates
/// than it can start inside its create deadline.
///
/// # Errors
///
/// A create call as a whole failed.
///
/// # Panics
///
/// A create task panicked, which is a bug in the SDK.
pub async fn fill(
    t: &Target,
    step: &str,
    n: u32,
    batch: u32,
) -> Result<(Vec<Cell>, Duration, Vec<Error>), Error> {
    let spec = Arc::new(t.spec(step));
    let started = Instant::now();
    let (mut cells, mut errors) = (Vec::new(), Vec::new());
    let mut calls = std::collections::VecDeque::new();
    let mut left = n;
    while left > 0 || !calls.is_empty() {
        if left > 0 && calls.len() < 2 {
            let count = left.min(batch);
            left -= count;
            let (client, spec) = (t.client.clone(), spec.clone());
            calls.push_back(tokio::spawn(
                async move { client.create_many(&spec, count, None).await },
            ));
            continue;
        }
        let call = calls.pop_front().expect("a call in flight");
        for r in call.await.expect("a create task panicked")? {
            match r {
                Ok(c) => cells.push(c),
                Err(e) => errors.push(e),
            }
        }
    }
    Ok((cells, started.elapsed(), errors))
}

/// What running a no-op command in cells looked like.
#[derive(Clone, Debug)]
pub struct ExecStep {
    /// How many commands were in flight at once.
    pub concurrency: usize,
    /// How many cells they were spread over.
    pub cells: usize,
    /// How many ran, and how many failed or exited non zero.
    pub ran: usize,
    /// Failures and non zero exits.
    pub failed: usize,
    /// The first failure, if any.
    pub first_error: Option<String>,
    /// Wall time for the whole step.
    pub wall: Duration,
    /// The round trip of every command that succeeded.
    pub latency: Option<Summary>,
    /// Every command, worker by worker.
    pub samples: Vec<Sample>,
}

impl ExecStep {
    /// Commands that succeeded per second of wall time.
    #[must_use]
    pub fn throughput(&self) -> f64 {
        (self.ran - self.failed) as f64 / self.wall.as_secs_f64()
    }
}

/// Runs `argv` in the cells for `seconds`, `concurrency` at a time, each worker going round the
/// cells from its own starting point. Closed loop, because what is measured here is how many the
/// node can turn around, and an open loop past that number only measures a queue.
///
/// # Panics
///
/// A worker panicked, which is a bug in the SDK.
pub async fn exec_step(
    cells: &[Cell],
    argv: &'static [&'static str],
    concurrency: usize,
    seconds: u32,
) -> ExecStep {
    let cells: Arc<Vec<Cell>> = Arc::new(cells.to_vec());
    let end = Instant::now() + Duration::from_secs(u64::from(seconds));
    let start = Instant::now();
    let mut workers = Vec::new();
    for w in 0..concurrency {
        let cells = cells.clone();
        workers.push(tokio::spawn(async move {
            let (mut ok, mut failed, mut first, mut samples) =
                (Vec::new(), 0usize, None, Vec::new());
            let mut i = w;
            while Instant::now() < end {
                let cell = &cells[i % cells.len()];
                i += concurrency;
                let t = Instant::now();
                let r = cell.run(argv.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()).await;
                let took = t.elapsed();
                let error = match r {
                    Ok(r) if r.exit_code == 0 => {
                        ok.push(took);
                        None
                    }
                    Ok(r) => Some(format!("exit {}", r.exit_code)),
                    Err(e) => Some(e.to_string()),
                };
                if let Some(e) = &error {
                    failed += 1;
                    first.get_or_insert_with(|| e.clone());
                }
                samples.push(Sample { at: t - start, took, error });
            }
            (ok, failed, first, samples)
        }));
    }
    let (mut ok, mut failed, mut first_error, mut samples) = (Vec::new(), 0, None, Vec::new());
    for w in workers {
        let (o, f, e, s) = w.await.expect("an exec worker panicked");
        ok.extend(o);
        failed += f;
        samples.extend(s);
        if first_error.is_none() {
            first_error = e;
        }
    }
    let wall = start.elapsed();
    ExecStep {
        concurrency,
        cells: cells.len(),
        ran: ok.len() + failed,
        failed,
        first_error,
        wall,
        latency: Summary::of(&ok),
        samples,
    }
}

/// A duration in milliseconds with one decimal, the way the report tables print them.
#[must_use]
pub fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

/// The markdown table rows for a create storm.
#[must_use]
pub fn storm_table(steps: &[StormStep]) -> String {
    let mut s = String::from(
        "| rate/s | started | failed | achieved/s | p50 ms | p90 ms | p99 ms | max ms | iqr ms | stop ms |\n",
    );
    s.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for st in steps {
        let (p50, p90, p99, max, iqr) = st.latency.map_or_else(
            || ("-".into(), "-".into(), "-".into(), "-".into(), "-".into()),
            |l| (ms(l.p50), ms(l.p90), ms(l.p99), ms(l.max), ms(l.iqr)),
        );
        let _ = writeln!(
            s,
            "| {} | {} | {} | {:.1} | {p50} | {p90} | {p99} | {max} | {iqr} | {} |",
            st.rate,
            st.started,
            st.failed,
            st.achieved(),
            ms(st.stopped.1)
        );
    }
    s
}

/// The markdown table rows for exec steps.
#[must_use]
pub fn exec_table(steps: &[ExecStep]) -> String {
    let mut s = String::from(
        "| in flight | cells | ran | failed | per second | p50 ms | p90 ms | p99 ms | max ms | iqr ms |\n",
    );
    s.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for st in steps {
        let (p50, p90, p99, max, iqr) = st.latency.map_or_else(
            || ("-".into(), "-".into(), "-".into(), "-".into(), "-".into()),
            |l| (ms(l.p50), ms(l.p90), ms(l.p99), ms(l.max), ms(l.iqr)),
        );
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {:.0} | {p50} | {p90} | {p99} | {max} | {iqr} |",
            st.concurrency,
            st.cells,
            st.ran,
            st.failed,
            st.throughput()
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(failed: usize, p99: u64) -> StormStep {
        let d = Duration::from_millis(p99);
        StormStep {
            rate: 10,
            started: 100,
            failed,
            first_error: None,
            infra: 0,
            wall: Duration::from_secs(10),
            latency: Summary::of(&[d]),
            stopped: (100, Duration::from_millis(5)),
            samples: Vec::new(),
        }
    }

    #[test]
    fn a_step_breaks_on_any_failure_or_a_slow_p99() {
        let limit = Duration::from_millis(400);
        assert!(!step(0, 399).broke(limit));
        assert!(step(0, 401).broke(limit));
        assert!(step(1, 10).broke(limit));
        assert!((step(0, 1).achieved() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn tables_have_a_row_per_step() {
        let t = storm_table(&[step(0, 12), step(2, 30)]);
        assert_eq!(t.lines().count(), 4);
        assert!(t.contains("| 10 | 100 | 2 | 9.8 | 30.0 |"), "{t}");
    }
}
