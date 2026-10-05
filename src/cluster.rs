//! The cluster suites that run through a gate: replay and node-loss.
//!
//! Both talk to the gate the way a user does, through `hive-sdk`, and read the keeper's applied
//! index straight from its `Status` call, since how many writes the keeper takes as load grows is
//! the number replay exists for. The gate spends a share of each quota it got from the keeper and
//! learns where cells are from the scout, so a create that goes well writes nothing to the keeper.
//! If that stops being true, the write rate climbs with the create rate and replay shows it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hive_proto::internal::StatusRequest;
use hive_proto::internal::keeper_client::KeeperClient;
use hive_sdk::{Backend, CellId, CellSpec, Client, Error, Reason, Source};
use tokio::sync::Mutex;

use crate::node::{Sample, ms};
use crate::stats::Summary;

/// A small, seeded generator, so that the same seed gives the same arrivals, lifetimes and images
/// from one run to the next. It is xorshift64*, which is plenty for picking workloads.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator from `seed`. Zero is moved off, since xorshift never leaves it.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    /// The next number.
    pub fn draw(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in the open interval from 0 to 1.
    pub fn unit(&mut self) -> f64 {
        ((self.draw() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// An exponential sample with this mean.
    pub fn exp(&mut self, mean: f64) -> f64 {
        -self.unit().ln() * mean
    }

    /// A standard normal sample, by Box and Muller.
    pub fn normal(&mut self) -> f64 {
        let (u, v) = (self.unit(), self.unit());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// The shape of the load replay offers. Arrivals come in bursts, a burst's size is geometric, a
/// cell lives for a lognormal time, and its image is picked with Zipf weights, so a few images
/// get most of the creates. These are the shapes the DSec paper reports for its production trace.
/// The parameters are flags, and a report prints the ones it used, because they are chosen to
/// look like that trace and are not fitted to it.
#[derive(Clone, Debug)]
pub struct Shape {
    /// The mean number of creates in a burst.
    pub burst: f64,
    /// The median lifetime.
    pub life: Duration,
    /// The sigma of the lifetime's logarithm.
    pub sigma: f64,
    /// The longest a cell is kept, whatever its draw.
    pub max_life: Duration,
    /// The images, most popular first.
    pub images: Vec<String>,
    /// The Zipf exponent over `images`.
    pub zipf: f64,
}

impl Shape {
    /// One burst: its size, and for each cell its lifetime and image.
    fn burst(&self, rng: &mut Rng) -> Vec<(Duration, usize)> {
        // A geometric size with mean `burst`, counting from one.
        let p = 1.0 / self.burst.max(1.0);
        let n = 1 + (rng.unit().ln() / (1.0 - p).ln()).floor().max(0.0) as usize;
        let n = if p >= 1.0 { 1 } else { n };
        let weights: Vec<f64> =
            (1..=self.images.len()).map(|r| 1.0 / (r as f64).powf(self.zipf)).collect();
        let total: f64 = weights.iter().sum();
        (0..n)
            .map(|_| {
                let life = self.life.as_secs_f64() * (self.sigma * rng.normal()).exp();
                let life = Duration::from_secs_f64(life).min(self.max_life);
                let mut pick = rng.unit() * total;
                let mut image = weights.len() - 1;
                for (i, w) in weights.iter().enumerate() {
                    if pick < *w {
                        image = i;
                        break;
                    }
                    pick -= w;
                }
                (life, image)
            })
            .collect()
    }
}

/// The keeper's applied index, which goes up by one for every write the cluster committed.
///
/// # Errors
///
/// The keeper could not be reached or did not answer.
pub async fn applied(keeper: &str) -> Result<u64, Error> {
    let fail = |e: String| Error::new(Reason::Internal, format!("keeper {keeper}: {e}"));
    let channel = tonic::transport::Endpoint::from_shared(keeper.to_string())
        .map_err(|e| fail(e.to_string()))?
        .connect()
        .await
        .map_err(|e| fail(e.to_string()))?;
    let mut k = KeeperClient::new(channel);
    let s = k.status(StatusRequest {}).await.map_err(|e| fail(e.message().to_string()))?;
    Ok(s.into_inner().applied)
}

/// Where replay and node-loss make their cells.
#[derive(Clone, Debug)]
pub struct Cluster {
    /// The client, connected to the gate and scoped to the run's project.
    pub client: Client,
    /// The keeper's address, for its applied index.
    pub keeper: String,
    /// The value of the `bench` label on every cell of this run.
    pub run: String,
}

impl Cluster {
    fn spec(&self, image: &str, step: &str) -> CellSpec {
        let mut spec = CellSpec::new(Source::Image(image.to_string()), Backend::Container);
        spec.labels = BTreeMap::from([
            ("bench".to_string(), self.run.clone()),
            ("step".to_string(), step.to_string()),
        ]);
        spec
    }
}

/// One step of replay.
#[derive(Clone, Debug)]
pub struct ReplayStep {
    /// The create rate offered, per second.
    pub rate: u32,
    /// How many creates were started.
    pub started: usize,
    /// How many failed, and how many of those were the platform's.
    pub failed: usize,
    /// Failures that belong to the platform rather than the caller.
    pub infra: usize,
    /// The first failure, if any.
    pub first_error: Option<String>,
    /// The latency of every create that succeeded.
    pub latency: Option<Summary>,
    /// The most cells alive at once, as the harness counted them.
    pub peak: usize,
    /// Keeper writes over the step, and the step's wall time.
    pub writes: u64,
    /// From the first create to the last stop.
    pub wall: Duration,
    /// Every create.
    pub samples: Vec<Sample>,
}

impl ReplayStep {
    /// Keeper writes per second over the step.
    #[must_use]
    pub fn write_rate(&self) -> f64 {
        self.writes as f64 / self.wall.as_secs_f64()
    }
}

/// Offers `rate` creates a second in bursts for `seconds`, keeps each cell for its lifetime and
/// stops it, and counts the keeper's writes from before the first create to after the last stop.
/// Open loop, as in create-storm.
///
/// # Errors
///
/// The keeper could not be read before or after.
///
/// # Panics
///
/// A create task panicked, which is a bug in the SDK.
pub async fn replay_step(
    c: &Cluster,
    shape: &Shape,
    rng: &mut Rng,
    rate: u32,
    seconds: u32,
) -> Result<ReplayStep, Error> {
    let before = applied(&c.keeper).await?;
    let started_at = tokio::time::Instant::now();
    let end = started_at + Duration::from_secs(u64::from(seconds));
    let alive = Arc::new(Mutex::new((0usize, 0usize)));
    let mut tasks = Vec::new();
    let mut at = started_at;
    let step = format!("replay-{rate}");
    let mean_gap = shape.burst.max(1.0) / f64::from(rate.max(1));
    loop {
        at += Duration::from_secs_f64(rng.exp(mean_gap));
        if at >= end {
            break;
        }
        tokio::time::sleep_until(at).await;
        for (life, image) in shape.burst(rng) {
            let (client, alive) = (c.client.clone(), alive.clone());
            let spec = c.spec(&shape.images[image], &step);
            let from = started_at.elapsed();
            tasks.push(tokio::spawn(async move {
                let t = Instant::now();
                let r = client.create(&spec).await;
                let took = t.elapsed();
                let r = match r {
                    Ok(cell) => {
                        {
                            let mut a = alive.lock().await;
                            a.0 += 1;
                            a.1 = a.1.max(a.0);
                        }
                        tokio::time::sleep(life).await;
                        let _ = cell.stop().await;
                        alive.lock().await.0 -= 1;
                        Ok(())
                    }
                    Err(e) => Err(e),
                };
                (from, took, r)
            }));
        }
    }
    let (mut ok, mut failed, mut infra, mut first_error) = (Vec::new(), 0, 0, None);
    let mut samples = Vec::with_capacity(tasks.len());
    let started = tasks.len();
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
    let wall = started_at.elapsed();
    let after = applied(&c.keeper).await?;
    let peak = alive.lock().await.1;
    Ok(ReplayStep {
        rate,
        started,
        failed,
        infra,
        first_error,
        latency: Summary::of(&ok),
        peak,
        writes: after.saturating_sub(before),
        wall,
        samples,
    })
}

/// The keeper's writes a second with nothing offered, over `seconds`: lease renewals and quota
/// slices, the floor every replay step sits on.
///
/// # Errors
///
/// The keeper could not be read.
pub async fn idle_writes(keeper: &str, seconds: u32) -> Result<f64, Error> {
    let before = applied(keeper).await?;
    let t = Instant::now();
    tokio::time::sleep(Duration::from_secs(u64::from(seconds))).await;
    let after = applied(keeper).await?;
    Ok(after.saturating_sub(before) as f64 / t.elapsed().as_secs_f64())
}

/// The markdown table for replay.
#[must_use]
pub fn replay_table(idle: f64, steps: &[ReplayStep]) -> String {
    let mut s = String::from(
        "| rate/s | started | failed | infra | peak alive | p50 ms | p99 ms | max ms | keeper writes/s |\n",
    );
    s.push_str("|---|---|---|---|---|---|---|---|---|\n");
    let _ = writeln!(s, "| 0 | 0 | 0 | 0 | 0 | - | - | - | {idle:.2} |");
    for st in steps {
        let (p50, p99, max) = st.latency.map_or_else(
            || ("-".into(), "-".into(), "-".into()),
            |l| (ms(l.p50), ms(l.p99), ms(l.max)),
        );
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {p50} | {p99} | {max} | {:.2} |",
            st.rate,
            st.started,
            st.failed,
            st.infra,
            st.peak,
            st.write_rate()
        );
    }
    s
}

/// How one create of node-loss ended, as the caller sees it after its retries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A cell was made, on this node.
    Made(CellId),
    /// Refused for a reason that is the caller's, like quota.
    Refused(Reason),
    /// Failed after every retry, for this reason.
    Failed(Reason),
}

/// What the gate said about one cell after the node went away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fate {
    /// Still running.
    Running,
    /// Answered with `CELL_LOST`, or ended with the node lost as its cause, either of which a
    /// trainer masks.
    Lost,
    /// Ended for any other cause.
    Ended,
    /// `CELL_NOT_FOUND`.
    NotFound,
    /// Any other answer, which means the cell was not classified.
    Unclassified,
}

/// What node-loss saw.
#[derive(Clone, Debug)]
pub struct NodeLoss {
    /// The node that was killed, and when, from the start of the burst.
    pub killed: (u16, Duration),
    /// Every create of the burst, in the order they were started, with its tries.
    pub outcomes: Vec<(Outcome, u32)>,
    /// The cells made before the burst, and those made by it.
    pub cells: Vec<CellId>,
    /// For each fate, the cells on the killed node and the cells elsewhere.
    pub fates: BTreeMap<Fate, (usize, usize)>,
    /// From the kill to the first `CELL_LOST` the gate gave for a cell on the killed node.
    pub detected: Option<Duration>,
    /// The latency of every create in the burst that made a cell, retries included.
    pub latency: Option<Summary>,
    /// Every create of the burst.
    pub samples: Vec<Sample>,
}

impl NodeLoss {
    /// Creates of the burst that ended without a cell for a reason that is the platform's. A
    /// trainer would see these, so the target is none.
    #[must_use]
    pub fn unmasked(&self) -> usize {
        self.outcomes.iter().filter(|(o, _)| matches!(o, Outcome::Failed(_))).count()
    }
}

/// Tries a keyed create up to `tries` times while the answer does not say whether a cell was
/// made, as a trainer's client would. The key makes a retry safe.
async fn keyed(client: &Client, spec: &CellSpec, key: &str, tries: u32) -> (Outcome, u32) {
    let mut last = Reason::Internal;
    for attempt in 1..=tries {
        let r = match client.create_many(spec, 1, Some(key)).await {
            Ok(mut v) => v.pop().unwrap_or_else(|| Err(Error::new(Reason::Internal, "no cell"))),
            Err(e) => Err(e),
        };
        match r {
            Ok(cell) => match cell.id().parse::<CellId>() {
                Ok(id) => return (Outcome::Made(id), attempt),
                Err(_) => return (Outcome::Failed(Reason::Internal), attempt),
            },
            Err(e) if !e.reason.is_infra() => return (Outcome::Refused(e.reason), attempt),
            Err(e) => {
                last = e.reason;
                if attempt < tries {
                    tokio::time::sleep(Duration::from_millis(200 << attempt)).await;
                }
            }
        }
    }
    (Outcome::Failed(last), tries)
}

/// The options node-loss runs with.
#[derive(Clone, Debug)]
pub struct LossOpts {
    /// The image every cell starts from.
    pub image: String,
    /// Cells made and held before the burst.
    pub hold: u32,
    /// The burst's create rate, and how long it lasts.
    pub rate: u32,
    /// How long the burst lasts.
    pub seconds: u32,
    /// When the node is killed, from the start of the burst.
    pub kill_after: Duration,
    /// The shell command that kills one comb.
    pub kill: String,
    /// The node that command kills.
    pub node: u16,
    /// How long to wait after the burst before asking about every cell.
    pub settle: Duration,
}

/// Holds cells across the cluster, starts a burst of keyed creates, kills a comb in the middle
/// of it, and then asks the gate about every cell. A cell on the killed node has to come back as
/// lost, and every create of the burst has to end with a cell, after its retries.
///
/// # Errors
///
/// The kill command could not be run, or making the held cells failed as a whole.
///
/// # Panics
///
/// A task panicked, which is a bug in the SDK.
pub async fn node_loss(c: &Cluster, o: &LossOpts) -> Result<NodeLoss, Error> {
    let spec = Arc::new(c.spec(&o.image, "loss"));
    let mut cells = Vec::new();
    for i in 0..o.hold {
        if let (Outcome::Made(id), _) =
            keyed(&c.client, &spec, &format!("{}-hold-{i}", c.run), 4).await
        {
            cells.push(id);
        }
    }
    let start = tokio::time::Instant::now();
    let total = o.rate * o.seconds;
    let gap = Duration::from_secs(1) / o.rate.max(1);
    let mut tasks = Vec::with_capacity(total as usize);
    let mut killed = None;
    for i in 0..total {
        let at = start + gap * i;
        if killed.is_none() && at >= start + o.kill_after {
            tokio::time::sleep_until(start + o.kill_after).await;
            killed = Some(kill(&o.kill, start).await?);
        }
        tokio::time::sleep_until(at).await;
        let (client, spec) = (c.client.clone(), spec.clone());
        let key = format!("{}-burst-{i}", c.run);
        let from = start.elapsed();
        tasks.push(tokio::spawn(async move {
            let t = Instant::now();
            let (outcome, tries) = keyed(&client, &spec, &key, 4).await;
            (from, t.elapsed(), outcome, tries)
        }));
    }
    let killed_at = match killed {
        Some(k) => k,
        None => kill(&o.kill, start).await?,
    };
    // Watch one held cell on the killed node, to time how long the gate takes to call it lost.
    let watched = cells.iter().copied().find(|id| id.node() == o.node);
    let watch = watched.map(|id| {
        let client = c.client.clone();
        tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline {
                if let Err(e) = client.get(&id.to_string()).await
                    && e.reason == Reason::CellLost
                {
                    return Some(Instant::now());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            None
        })
    });
    let (mut outcomes, mut ok, mut samples) = (Vec::new(), Vec::new(), Vec::new());
    for task in tasks {
        let (at, took, outcome, tries) = task.await.expect("a create task panicked");
        let error = match &outcome {
            Outcome::Made(id) => {
                ok.push(took);
                cells.push(*id);
                None
            }
            Outcome::Refused(r) | Outcome::Failed(r) => Some(format!("{r:?}")),
        };
        samples.push(Sample { at, took, error });
        outcomes.push((outcome, tries));
    }
    tokio::time::sleep(o.settle).await;
    let detected = match watch {
        Some(w) => w
            .await
            .expect("the watch task panicked")
            .map(|at| at.saturating_duration_since(killed_at.1)),
        None => None,
    };
    let mut fates: BTreeMap<Fate, (usize, usize)> = BTreeMap::new();
    for id in &cells {
        let fate = match c.client.get(&id.to_string()).await {
            Ok(cell) => {
                use hive_sdk::v1::{Cause, CellState};
                match cell.state() {
                    CellState::Pending
                    | CellState::Preparing
                    | CellState::Starting
                    | CellState::Running => Fate::Running,
                    CellState::Unspecified => Fate::Unclassified,
                    _ if cell.info().cause() == Cause::NodeLost => Fate::Lost,
                    _ => Fate::Ended,
                }
            }
            Err(e) if e.reason == Reason::CellLost => Fate::Lost,
            Err(e) if e.reason == Reason::CellNotFound => Fate::NotFound,
            Err(_) => Fate::Unclassified,
        };
        let slot = fates.entry(fate).or_default();
        if id.node() == o.node {
            slot.0 += 1;
        } else {
            slot.1 += 1;
        }
    }
    Ok(NodeLoss {
        killed: (o.node, killed_at.0),
        outcomes,
        cells,
        fates,
        detected,
        latency: Summary::of(&ok),
        samples,
    })
}

/// Runs the kill command and returns when it ran, from `start` and as an instant.
async fn kill(cmd: &str, start: tokio::time::Instant) -> Result<(Duration, Instant), Error> {
    let at = (start.elapsed(), Instant::now());
    let out = tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await;
    match out {
        Ok(o) if o.status.success() => Ok(at),
        Ok(o) => Err(Error::new(
            Reason::InvalidArgument,
            format!("`{cmd}` exited {}: {}", o.status, String::from_utf8_lossy(&o.stderr).trim()),
        )),
        Err(e) => Err(Error::new(Reason::InvalidArgument, format!("`{cmd}`: {e}"))),
    }
}

/// The markdown for node-loss.
#[must_use]
pub fn loss_report(l: &NodeLoss) -> String {
    let mut s = String::new();
    let made = l.outcomes.iter().filter(|(o, _)| matches!(o, Outcome::Made(_))).count();
    let refused = l.outcomes.iter().filter(|(o, _)| matches!(o, Outcome::Refused(_))).count();
    let retried =
        l.outcomes.iter().filter(|(o, t)| matches!(o, Outcome::Made(_)) && *t > 1).count();
    let _ = writeln!(
        s,
        "node {} killed {} ms into the burst. {} creates in the burst: {made} made a cell, {retried} of them after a retry, {refused} refused, {} failed after every retry.",
        l.killed.0,
        ms(l.killed.1),
        l.outcomes.len(),
        l.unmasked()
    );
    if let Some(lat) = l.latency {
        let _ = writeln!(s, "\nburst create latency, retries included: {lat}");
    }
    match l.detected {
        Some(d) => {
            let _ = writeln!(
                s,
                "\nthe gate answered CELL_LOST for a cell on the killed node {} ms after the kill.",
                ms(d)
            );
        }
        None => {
            let _ = writeln!(
                s,
                "\nthe gate never answered CELL_LOST for the watched cell on the killed node."
            );
        }
    }
    s.push_str("\n| fate | on the killed node | elsewhere |\n|---|---|---|\n");
    for (fate, (on, off)) in &l.fates {
        let _ = writeln!(s, "| {fate:?} | {on} | {off} |");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_workload() {
        let shape = Shape {
            burst: 4.0,
            life: Duration::from_secs(20),
            sigma: 1.0,
            max_life: Duration::from_secs(120),
            images: vec!["a".into(), "b".into(), "c".into()],
            zipf: 1.1,
        };
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        for _ in 0..100 {
            assert_eq!(shape.burst(&mut a), shape.burst(&mut b));
        }
    }

    #[test]
    fn bursts_lifetimes_and_images_have_the_shape_asked_for() {
        let shape = Shape {
            burst: 4.0,
            life: Duration::from_secs(20),
            sigma: 1.0,
            max_life: Duration::from_secs(120),
            images: vec!["a".into(), "b".into(), "c".into()],
            zipf: 1.1,
        };
        let mut rng = Rng::new(42);
        let (mut cells, mut bursts, mut lives, mut first) = (0usize, 0usize, Vec::new(), 0usize);
        for _ in 0..20_000 {
            let b = shape.burst(&mut rng);
            bursts += 1;
            cells += b.len();
            for (life, image) in b {
                lives.push(life);
                first += usize::from(image == 0);
            }
        }
        let mean = cells as f64 / bursts as f64;
        assert!((3.8..4.2).contains(&mean), "mean burst {mean}");
        lives.sort_unstable();
        let median = lives[lives.len() / 2].as_secs_f64();
        assert!((19.0..21.0).contains(&median), "median life {median}");
        assert!(lives.iter().all(|l| *l <= Duration::from_secs(120)));
        // With exponent 1.1 over three images the first has 1 / (1 + 2^-1.1 + 3^-1.1) of them.
        let share = first as f64 / cells as f64;
        assert!((0.53..0.58).contains(&share), "first image share {share}");
    }
}
