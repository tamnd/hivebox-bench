//! The `hivebox-bench` command.

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::time::Duration;

use hive_sdk::Client;
use hivebox_bench::cluster::{self, Cluster, LossOpts, Rng, Shape};
use hivebox_bench::density::{self, DensityOpts};
use hivebox_bench::image::{self, Nectar};
use hivebox_bench::memory::{self, MemOpts};
use hivebox_bench::node::{self, Target};
use hivebox_bench::qos::{self, QosOpts};
use hivebox_bench::raw::Raw;
use hivebox_bench::snap::{self, SnapOpts};
use hivebox_bench::suite::{self, SUITES};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => {
            println!("{:<20} {:<8} {:<4} target", "suite", "scope", "from");
            for s in SUITES {
                let scope = format!("{:?}", s.scope).to_lowercase();
                println!("{:<20} {:<8} {:<4} {}", s.name, scope, s.milestone, s.target);
            }
            ExitCode::SUCCESS
        }
        Some("run") => match args.get(1).and_then(|n| suite::find(n)) {
            Some(s)
                if matches!(
                    s.name,
                    "create-storm"
                        | "exec"
                        | "replay"
                        | "node-loss"
                        | "cold-image"
                        | "image-import"
                        | "cpu-qos"
                        | "snapshot"
                        | "memory"
                        | "density"
                ) =>
            {
                let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build();
                let rt = rt.expect("a tokio runtime");
                match rt.block_on(run(s.name, &Opts::parse(&args[2..]))) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("{}: {e}", s.name);
                        ExitCode::FAILURE
                    }
                }
            }
            Some(s) => {
                eprintln!(
                    "{} measures {}, and it needs a hivebox cluster at {} or later, which this harness does not drive yet",
                    s.name, s.measures, s.milestone
                );
                ExitCode::FAILURE
            }
            None => {
                eprintln!("no such suite, try `hivebox-bench list`");
                ExitCode::FAILURE
            }
        },
        Some("--version" | "-V") => {
            println!("hivebox-bench {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            println!("hivebox-bench <command>");
            println!();
            println!(
                "  list         every suite, where it runs, and the target it is judged against"
            );
            println!("  run <suite>  run one suite against the comb or gate in HIVEBOX_ENDPOINT");
            println!();
            println!(
                "HIVEBOX_TOKEN is sent as the bearer token when it is set, which a gate needs."
            );
            println!();
            println!("options for run, with their defaults:");
            println!("  --image python          the image every cell starts from");
            println!("  --seconds 10            how long each step lasts");
            println!("  --raw FILE              also write every call to FILE as zstd JSON lines");
            println!(
                "  --rates 25,50,100,200,300,400  create-storm and replay: the rates to step through"
            );
            println!("  --p99-ms 400            create-storm: the p99 past which a step breaks");
            println!("  --alive 0               create-storm: also hold this many cells at once");
            println!("  --cells 100             exec: how many cells the commands go round");
            println!("  --in-flight 1,8,32,128  exec: the concurrency levels to step through");
            println!(
                "  --keeper http://127.0.0.1:7480  replay: the keeper whose writes are counted"
            );
            println!("  --idle-s 30             replay: how long the idle write rate is measured");
            println!("  --burst 4               replay: the mean creates in a burst");
            println!("  --life-s 20             replay: the median lifetime of a cell");
            println!("  --sigma 1.0             replay: the sigma of the lifetime's logarithm");
            println!("  --max-life-s 120        replay: the longest a cell is kept");
            println!(
                "  --images IMAGE,...      replay: the images, most popular first, default --image"
            );
            println!("  --zipf 1.1              replay: the Zipf exponent over the images");
            println!(
                "  --seed 1                replay, density: the seed for arrivals, lifetimes, images and CPU shares"
            );
            println!("  --hold 30               node-loss: cells made and held before the burst");
            println!("  --rate 10               node-loss: the burst's creates a second");
            println!("  --kill-after-s 5        node-loss: when in the burst the comb is killed");
            println!("  --kill CMD              node-loss: the shell command that kills one comb");
            println!("  --node N                node-loss: the node that command kills");
            println!(
                "  --settle-s 20           node-loss: the wait after the burst before checking cells"
            );
            println!("  --nectar hive-nectar    cold-image, image-import: the hive-nectar binary");
            println!("  --store DIR             cold-image, image-import: the store directory");
            println!("  --s3 URL                cold-image: a bucket in place of --store");
            println!(
                "  --work DIR              cold-image, image-import: where imports keep built layers"
            );
            println!("  --layout DIR            cold-image: the OCI layout of the image to start");
            println!(
                "  --cmd CMD               cold-image: what to run in it, default `python3 -c 1`"
            );
            println!("  --repeat 10             cold-image: starts per mode");
            println!("  --scratch DIR           cold-image: where each start gets an empty cache");
            println!(
                "  --layouts FILE          image-import: the OCI layouts to import, one per line"
            );
            println!("  --hogs 4                cpu-qos: hog cells next to the probe");
            println!(
                "  --loops 2               cpu-qos: spinning processes, and cores, per hog cell"
            );
            println!("  --steps 200             cpu-qos: probe steps per mode per round");
            println!("  --turns 200000          cpu-qos: Python loop turns in one probe step");
            println!("  --pause-ms 20           cpu-qos: the probe's sleep after each step");
            println!("  --hog-s 30              cpu-qos: how long the hogs spin each time");
            println!("  --rounds 5              cpu-qos: rounds of alone, no QoS and QoS");
            println!("  --pauses 50             snapshot: pause and resume rounds");
            println!(
                "  --sizes 10,100,1000     snapshot: the MiB a cell writes before its snapshot"
            );
            println!("  --snapshots 3           snapshot: snapshots per size");
            println!("  --agents 32             memory: agent cells");
            println!("  --think-s 60            memory: how long an agent waits after each step");
            println!("  --length-s 600          memory: how long memory is read, after a warm up");
            println!(
                "  --scratch-mib 32        memory: the scratch file each agent reads every step"
            );
            println!(
                "  --cgroup DIR            memory, density: the comb's cgroup root, which is read"
            );
            println!("  --counts 25,50,...,800  density: the cells alive in each step");
            println!(
                "  --load 1                density: what each CPU share is multiplied by, 0 for none"
            );
            println!("  --probes 400            density: timed `true` runs per step");
            println!("  --parallel 4            density: of those, how many at once");
            println!(
                "  --warm-s 5              density: how long the loaders run before the probes"
            );
            println!("  --cell-mib 256          density: the memory each cell asks for");
            println!(
                "  --floor-mib 2048        density: the host memory available a step needs to start"
            );
            ExitCode::SUCCESS
        }
    }
}

#[derive(Debug)]
struct Opts {
    image: String,
    seconds: u32,
    rates: Option<Vec<u32>>,
    p99: Duration,
    alive: u32,
    cells: u32,
    in_flight: Vec<usize>,
    raw: Option<std::path::PathBuf>,
    keeper: String,
    idle: u32,
    shape: Shape,
    seed: u64,
    loss: LossOpts,
    nectar: std::path::PathBuf,
    store: Option<String>,
    s3: Option<String>,
    work: Option<std::path::PathBuf>,
    layout: Option<std::path::PathBuf>,
    cmd: String,
    repeat: usize,
    scratch: Option<std::path::PathBuf>,
    layouts: Option<std::path::PathBuf>,
    qos: QosOpts,
    snap: SnapOpts,
    mem: MemOpts,
    density: DensityOpts,
}

impl Opts {
    fn parse(args: &[String]) -> Self {
        let mut o = Self {
            image: "python".into(),
            seconds: 10,
            rates: None,
            p99: Duration::from_millis(400),
            alive: 0,
            cells: 100,
            in_flight: vec![1, 8, 32, 128],
            raw: None,
            keeper: "http://127.0.0.1:7480".into(),
            idle: 30,
            shape: Shape {
                burst: 4.0,
                life: Duration::from_secs(20),
                sigma: 1.0,
                max_life: Duration::from_secs(120),
                images: Vec::new(),
                zipf: 1.1,
            },
            seed: 1,
            loss: LossOpts {
                image: String::new(),
                hold: 30,
                rate: 10,
                seconds: 10,
                kill_after: Duration::from_secs(5),
                kill: String::new(),
                node: 0,
                settle: Duration::from_secs(20),
            },
            nectar: "hive-nectar".into(),
            store: None,
            s3: None,
            work: None,
            layout: None,
            cmd: "python3 -c 1".into(),
            repeat: 10,
            scratch: None,
            layouts: None,
            qos: QosOpts {
                hogs: 4,
                loops: 2,
                steps: 200,
                work: 200_000,
                pause: Duration::from_millis(20),
                hog_for: Duration::from_secs(30),
                rounds: 5,
            },
            snap: SnapOpts { pauses: 50, sizes: vec![10, 100, 1000], repeat: 3 },
            mem: MemOpts {
                agents: 32,
                think: Duration::from_secs(60),
                length: Duration::from_secs(600),
                scratch: 32,
                cgroup: std::path::PathBuf::new(),
            },
            density: DensityOpts {
                counts: vec![25, 50, 100, 200, 400, 800],
                probes: 400,
                parallel: 4,
                warm: Duration::from_secs(5),
                cell_mib: 256,
                floor_mib: 2048,
                cgroup: None,
                seed: 1,
                load: 1.0,
            },
        };
        let list = |v: &str| v.split(',').map(|n| n.trim().parse().expect("a number")).collect();
        let secs = |v: &str| Duration::from_secs_f64(v.parse().expect("a number of seconds"));
        for pair in args.chunks(2) {
            let v = pair.get(1).map_or("", String::as_str);
            match pair[0].as_str() {
                "--image" => o.image = v.into(),
                "--seconds" => o.seconds = v.parse().expect("--seconds takes a number"),
                "--rates" => o.rates = Some(list(v)),
                "--p99-ms" => o.p99 = Duration::from_millis(v.parse().expect("a number")),
                "--alive" => o.alive = v.parse().expect("--alive takes a number"),
                "--cells" => o.cells = v.parse().expect("--cells takes a number"),
                "--in-flight" => {
                    o.in_flight = list(v).into_iter().map(|n: u32| n as usize).collect()
                }
                "--raw" => o.raw = Some(v.into()),
                "--keeper" => o.keeper = v.into(),
                "--idle-s" => o.idle = v.parse().expect("--idle-s takes a number"),
                "--burst" => o.shape.burst = v.parse().expect("--burst takes a number"),
                "--life-s" => o.shape.life = secs(v),
                "--sigma" => o.shape.sigma = v.parse().expect("--sigma takes a number"),
                "--max-life-s" => o.shape.max_life = secs(v),
                "--images" => o.shape.images = v.split(',').map(str::to_string).collect(),
                "--zipf" => o.shape.zipf = v.parse().expect("--zipf takes a number"),
                "--seed" => o.seed = v.parse().expect("--seed takes a number"),
                "--hold" => o.loss.hold = v.parse().expect("--hold takes a number"),
                "--rate" => o.loss.rate = v.parse().expect("--rate takes a number"),
                "--kill-after-s" => o.loss.kill_after = secs(v),
                "--kill" => o.loss.kill = v.into(),
                "--node" => o.loss.node = v.parse().expect("--node takes a number"),
                "--settle-s" => o.loss.settle = secs(v),
                "--nectar" => o.nectar = v.into(),
                "--store" => o.store = Some(v.into()),
                "--s3" => o.s3 = Some(v.into()),
                "--work" => o.work = Some(v.into()),
                "--layout" => o.layout = Some(v.into()),
                "--cmd" => o.cmd = v.into(),
                "--repeat" => o.repeat = v.parse().expect("--repeat takes a number"),
                "--scratch" => o.scratch = Some(v.into()),
                "--layouts" => o.layouts = Some(v.into()),
                "--hogs" => o.qos.hogs = v.parse().expect("--hogs takes a number"),
                "--loops" => o.qos.loops = v.parse().expect("--loops takes a number"),
                "--steps" => o.qos.steps = v.parse().expect("--steps takes a number"),
                "--turns" => o.qos.work = v.parse().expect("--turns takes a number"),
                "--pause-ms" => o.qos.pause = Duration::from_millis(v.parse().expect("a number")),
                "--hog-s" => o.qos.hog_for = secs(v),
                "--rounds" => o.qos.rounds = v.parse().expect("--rounds takes a number"),
                "--pauses" => o.snap.pauses = v.parse().expect("--pauses takes a number"),
                "--sizes" => o.snap.sizes = list(v),
                "--snapshots" => o.snap.repeat = v.parse().expect("--snapshots takes a number"),
                "--agents" => o.mem.agents = v.parse().expect("--agents takes a number"),
                "--think-s" => o.mem.think = secs(v),
                "--length-s" => o.mem.length = secs(v),
                "--scratch-mib" => o.mem.scratch = v.parse().expect("--scratch-mib takes a number"),
                "--cgroup" => {
                    o.mem.cgroup = v.into();
                    o.density.cgroup = Some(v.into());
                }
                "--counts" => o.density.counts = list(v),
                "--load" => o.density.load = v.parse().expect("--load takes a number"),
                "--probes" => o.density.probes = v.parse().expect("--probes takes a number"),
                "--parallel" => o.density.parallel = v.parse().expect("--parallel takes a number"),
                "--warm-s" => o.density.warm = secs(v),
                "--cell-mib" => o.density.cell_mib = v.parse().expect("--cell-mib takes a number"),
                "--floor-mib" => {
                    o.density.floor_mib = v.parse().expect("--floor-mib takes a number")
                }
                other => panic!("unknown option {other}"),
            }
        }
        if o.shape.images.is_empty() {
            o.shape.images = vec![o.image.clone()];
        }
        o.loss.image = o.image.clone();
        o.loss.seconds = o.seconds;
        o.density.seed = o.seed;
        o
    }
}

fn bad(what: impl Into<String>) -> hive_sdk::Error {
    hive_sdk::Error::new(hive_sdk::Reason::InvalidArgument, what)
}

async fn run(suite: &str, o: &Opts) -> Result<(), hive_sdk::Error> {
    if matches!(suite, "cold-image" | "image-import") {
        println!("{}", machine());
        return images(suite, o);
    }
    let endpoint = std::env::var("HIVEBOX_ENDPOINT")
        .unwrap_or_else(|_| "unix:/run/hivebox/comb.sock".to_string());
    let run = format!("{}-{}", suite, std::process::id());
    let mut client = Client::connect(&endpoint).await?.project("bench")?;
    if let Ok(token) = std::env::var("HIVEBOX_TOKEN") {
        client = client.token(&token)?;
    }
    let t = Target { client, image: o.image.clone(), run };
    println!("{}", machine());
    println!("endpoint {endpoint}, image {}, run {}", o.image, t.run);
    println!();
    let mut raw = match &o.raw {
        Some(path) => Some(Raw::create(path).map_err(|e| bad(format!("{}: {e}", path.display())))?),
        None => None,
    };
    let c = Cluster { client: t.client.clone(), keeper: o.keeper.clone(), run: t.run.clone() };
    let r = match suite {
        "create-storm" => storm(&t, o, &mut raw).await,
        "replay" => replay(&c, o, &mut raw).await,
        "node-loss" => loss(&c, o, &mut raw).await,
        "cpu-qos" => cpu_qos(&t, o, &mut raw).await,
        "snapshot" => snapshot(&t, o, &mut raw).await,
        "memory" => mem(&t, o, &mut raw).await,
        "density" => dense(&t, o, &mut raw).await,
        _ => exec(&t, o, &mut raw).await,
    };
    // Whatever happened, leave nothing behind.
    let (left, took) = t.stop_all().await?;
    if left > 0 {
        eprintln!("stopped {left} cells left from the run in {} ms", node::ms(took));
    }
    r
}

async fn cpu_qos(t: &Target, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    let q = &o.qos;
    println!(
        "{} hog cells of {} spinning processes each, probe steps of {} Python loop turns with {} ms between, {} steps per mode, {} rounds",
        q.hogs,
        q.loops,
        q.work,
        q.pause.as_millis(),
        q.steps,
        q.rounds
    );
    let modes = qos::run(t, q).await?;
    for m in &modes {
        keep(raw, "cpu-qos", m.name, &m.samples());
        if m.overran > 0 {
            eprintln!("{}: the probe ran past the hogs in {} rounds", m.name, m.overran);
        }
    }
    println!();
    print!("{}", qos::table(&modes));
    Ok(())
}

async fn snapshot(t: &Target, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    let s = &o.snap;
    println!(
        "{} pause and resume rounds, {} snapshots at each of {:?} MiB written, fork not measured",
        s.pauses, s.repeat, s.sizes
    );
    let r = snap::run(t, s).await?;
    keep(raw, "snapshot", "idle", &snap::samples(&r.idle));
    keep(raw, "snapshot", "pause", &snap::samples(&r.pauses.pause));
    keep(raw, "snapshot", "resume", &snap::samples(&r.pauses.resume));
    for z in &r.sizes {
        keep(raw, "snapshot", &format!("snapshot-{}", z.mib), &snap::samples(&z.snapshot));
        keep(raw, "snapshot", &format!("stall-{}", z.mib), &snap::samples(&z.stall));
        keep(raw, "snapshot", &format!("restore-{}", z.mib), &snap::samples(&z.restore));
    }
    println!();
    print!("{}", snap::table(&r));
    Ok(())
}

async fn mem(t: &Target, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    let m = &o.mem;
    if m.cgroup.as_os_str().is_empty() {
        return Err(bad("memory needs --cgroup, the comb's cgroup root"));
    }
    println!(
        "{} agents, {} s of think time after each step, {} MiB scratch each, {} s measured after {} s of warm up, cgroup {}",
        m.agents,
        m.think.as_secs_f64(),
        m.scratch,
        m.length.as_secs_f64(),
        m.think.as_secs_f64(),
        m.cgroup.display()
    );
    let r = memory::run(t, m).await?;
    keep(raw, "memory", "step", &r.steps);
    if r.failed.0 > 0 {
        eprintln!("{} steps failed, first: {}", r.failed.0, r.failed.1.as_deref().unwrap_or(""));
    }
    println!();
    print!("{}", memory::table(&r));
    Ok(())
}

async fn dense(t: &Target, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    let d = &o.density;
    println!(
        "steps of {:?} cells of {} MiB, {} timed `true` runs per step {} at a time after {} s of load, CPU shares from seed {} times {}",
        d.counts,
        d.cell_mib,
        d.probes,
        d.parallel,
        d.warm.as_secs_f64(),
        d.seed,
        d.load
    );
    let r = density::run(t, d).await?;
    for s in &r.steps {
        keep(raw, "density", &s.cells.to_string(), &s.execs);
    }
    println!();
    print!("{}", density::table(&r));
    println!();
    if let Some(why) = &r.stopped {
        println!("stopped early: {why}");
    }
    match density::most(&r) {
        Some(n) => println!(
            "most cells with an exec p99 of {} ms or less: {n}",
            density::TARGET.as_millis()
        ),
        None => println!("no step had an exec p99 of {} ms or less", density::TARGET.as_millis()),
    }
    Ok(())
}

async fn replay(c: &Cluster, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    let s = &o.shape;
    println!(
        "bursts of {} on average, lifetimes lognormal with median {} s and sigma {} capped at {} s, {} images with Zipf {}, seed {}",
        s.burst,
        s.life.as_secs_f64(),
        s.sigma,
        s.max_life.as_secs_f64(),
        s.images.len(),
        s.zipf,
        o.seed
    );
    let idle = cluster::idle_writes(&c.keeper, o.idle).await?;
    eprintln!("idle: {idle:.2} keeper writes/s");
    let mut rng = Rng::new(o.seed);
    let mut steps = Vec::new();
    for &rate in o.rates.as_deref().unwrap_or(&[5, 10, 20, 40]) {
        let step = cluster::replay_step(c, s, &mut rng, rate, o.seconds).await?;
        if let Some(e) = &step.first_error {
            eprintln!("rate {rate}: {} failed ({} infra), first: {e}", step.failed, step.infra);
        }
        keep(raw, "replay", &rate.to_string(), &step.samples);
        eprint!("{}", cluster::replay_table(idle, std::slice::from_ref(&step)));
        steps.push(step);
    }
    println!();
    println!(
        "replay, {} s of arrivals a step, each step until its last cell is stopped",
        o.seconds
    );
    println!();
    print!("{}", cluster::replay_table(idle, &steps));
    Ok(())
}

async fn loss(c: &Cluster, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    if o.loss.kill.is_empty() {
        return Err(bad("node-loss needs --kill CMD and --node N"));
    }
    let l = cluster::node_loss(c, &o.loss).await?;
    keep(raw, "node-loss", "burst", &l.samples);
    println!(
        "{} cells held, then {} creates a second for {} s, keyed and retried up to 4 times",
        o.loss.hold, o.loss.rate, o.loss.seconds
    );
    println!();
    print!("{}", cluster::loss_report(&l));
    Ok(())
}

fn images(suite: &str, o: &Opts) -> Result<(), hive_sdk::Error> {
    let store = match (&o.store, &o.s3) {
        (Some(d), _) => vec!["--store".to_string(), d.clone()],
        (None, Some(u)) => vec!["--s3".to_string(), u.clone()],
        (None, None) => return Err(bad(format!("{suite} needs --store DIR or --s3 URL"))),
    };
    let work = o.work.clone().ok_or_else(|| bad(format!("{suite} needs --work DIR")))?;
    let n = Nectar { bin: o.nectar.clone(), store, work: work.clone() };
    if suite == "image-import" {
        let dir = o.store.as_deref().ok_or_else(|| bad("image-import measures a --store DIR"))?;
        let file = o.layouts.as_ref().ok_or_else(|| bad("image-import needs --layouts FILE"))?;
        let list =
            std::fs::read_to_string(file).map_err(|e| bad(format!("{}: {e}", file.display())))?;
        let layouts: Vec<std::path::PathBuf> =
            list.lines().map(str::trim).filter(|l| !l.is_empty()).map(Into::into).collect();
        let rows = image::import_all(&n, std::path::Path::new(dir), &layouts);
        println!();
        print!("{}", image::import_table(&rows, image::tree_size(&work).0));
        return Ok(());
    }
    let layout = o.layout.as_ref().ok_or_else(|| bad("cold-image needs --layout DIR"))?;
    let scratch = o.scratch.as_ref().ok_or_else(|| bad("cold-image needs --scratch DIR"))?;
    let (id, said, took) = n.import(layout).map_err(bad)?;
    println!("imported {} as {id} in {:.1} s: {said}", layout.display(), took.as_secs_f64());
    let traced = image::cold_runs_trace(&n, &id, &o.cmd, scratch).map_err(bad)?;
    println!("traced `{}` into {traced}", o.cmd);
    let modes = [
        image::cold_runs(&n, "whole", &id, "whole", &o.cmd, o.repeat, scratch),
        image::cold_runs(&n, "lazy", &id, "lazy", &o.cmd, o.repeat, scratch),
        image::cold_runs(&n, "lazy-traced", &traced, "lazy", &o.cmd, o.repeat, scratch),
    ];
    for m in &modes {
        if let (k, Some(e)) = &m.failed {
            eprintln!("{}: {k} failed, first: {e}", m.name);
        }
    }
    println!();
    println!("`{}` in an image with nothing of it on the node, {} starts a mode", o.cmd, o.repeat);
    println!();
    print!("{}", image::cold_table(&modes));
    Ok(())
}

/// Writes `samples` to the raw results file when there is one. A write that fails is said once
/// and the file is dropped, since the tables are still good without it.
fn keep(raw: &mut Option<Raw>, suite: &str, step: &str, samples: &[node::Sample]) {
    if let Some(r) = raw
        && let Err(e) = r.write(suite, step, samples)
    {
        eprintln!("raw results: {e}, not writing any more of them");
        *raw = None;
    }
}

async fn storm(t: &Target, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    let mut steps = Vec::new();
    for &rate in o.rates.as_deref().unwrap_or(&[25, 50, 100, 200, 300, 400]) {
        let step = node::storm_step(t, rate, o.seconds).await?;
        if let Some(e) = &step.first_error {
            eprintln!("rate {rate}: {} failed ({} infra), first: {e}", step.failed, step.infra);
        }
        keep(raw, "create-storm", &rate.to_string(), &step.samples);
        let broke = step.broke(o.p99);
        steps.push(step);
        eprint!("{}", node::storm_table(&steps[steps.len() - 1..]));
        if broke {
            break;
        }
    }
    println!("create storm, container, warm, {} s a step", o.seconds);
    println!();
    print!("{}", node::storm_table(&steps));
    if o.alive > 0 {
        let (cells, took, errors) = node::fill(t, "alive", o.alive, 50).await?;
        println!();
        println!(
            "{} cells asked for in batches of 50, two at a time: {} running after {} ms, {} failed",
            o.alive,
            cells.len(),
            node::ms(took),
            errors.len()
        );
        if let Some(e) = errors.first() {
            println!("first failure: {e}");
        }
        let step = node::exec_step(&cells, &["true"], 64, o.seconds).await;
        keep(raw, "create-storm", "alive-exec-64", &step.samples);
        println!();
        println!("`true` in all of them, 64 in flight, {} s", o.seconds);
        println!();
        print!("{}", node::exec_table(&[step]));
        let (n, took) = t.stop_all().await?;
        println!();
        println!("stopped {n} in {} ms", node::ms(took));
    }
    Ok(())
}

async fn exec(t: &Target, o: &Opts, raw: &mut Option<Raw>) -> Result<(), hive_sdk::Error> {
    let (cells, took, errors) = node::fill(t, "exec", o.cells, 50).await?;
    if let Some(e) = errors.first() {
        return Err(e.clone());
    }
    println!("{} cells made in {} ms", cells.len(), node::ms(took));
    // One short step first so the drones and the connection are warm.
    let _ = node::exec_step(&cells, &["true"], 8, 1).await;
    let mut steps = Vec::new();
    for &c in &o.in_flight {
        let step = node::exec_step(&cells, &["true"], c, o.seconds).await;
        if let Some(e) = &step.first_error {
            eprintln!("{c} in flight: {} failed, first: {e}", step.failed);
        }
        eprint!("{}", node::exec_table(std::slice::from_ref(&step)));
        keep(raw, "exec", &c.to_string(), &step.samples);
        steps.push(step);
    }
    println!();
    println!("no-op exec (`true`), warm, {} s a step", o.seconds);
    println!();
    print!("{}", node::exec_table(&steps));
    Ok(())
}

/// The machine line every report starts with, from what the kernel says about itself.
fn machine() -> String {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let cpu = read("/proc/cpuinfo");
    let model = cpu
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .map_or("unknown cpu", |l| l.trim_start_matches([' ', '\t', ':']));
    let cores = cpu.lines().filter(|l| l.starts_with("processor")).count();
    let mem = read("/proc/meminfo")
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.trim().trim_end_matches(" kB").parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024 / 1024);
    let kernel = read("/proc/sys/kernel/osrelease");
    let load = read("/proc/loadavg");
    let load: Vec<&str> = load.split_whitespace().take(3).collect();
    format!("{model}, {cores} cores, {mem} GiB, kernel {}, load {}", kernel.trim(), load.join(" "))
}
