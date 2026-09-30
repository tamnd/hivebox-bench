//! The `hivebox-bench` command.

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::time::Duration;

use hive_sdk::Client;
use hivebox_bench::node::{self, Target};
use hivebox_bench::raw::Raw;
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
            Some(s) if matches!(s.name, "create-storm" | "exec") => {
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
            println!("  run <suite>  run one suite against the comb in HIVEBOX_ENDPOINT");
            println!();
            println!("options for run, with their defaults:");
            println!("  --image python          the image every cell starts from");
            println!("  --seconds 10            how long each step lasts");
            println!("  --rates 25,50,100,200,300,400  create-storm: the rates to step through");
            println!("  --p99-ms 400            create-storm: the p99 past which a step breaks");
            println!("  --alive 0               create-storm: also hold this many cells at once");
            println!("  --cells 100             exec: how many cells the commands go round");
            println!("  --in-flight 1,8,32,128  exec: the concurrency levels to step through");
            println!("  --raw FILE              also write every call to FILE as zstd JSON lines");
            ExitCode::SUCCESS
        }
    }
}

#[derive(Debug)]
struct Opts {
    image: String,
    seconds: u32,
    rates: Vec<u32>,
    p99: Duration,
    alive: u32,
    cells: u32,
    in_flight: Vec<usize>,
    raw: Option<std::path::PathBuf>,
}

impl Opts {
    fn parse(args: &[String]) -> Self {
        let mut o = Self {
            image: "python".into(),
            seconds: 10,
            rates: vec![25, 50, 100, 200, 300, 400],
            p99: Duration::from_millis(400),
            alive: 0,
            cells: 100,
            in_flight: vec![1, 8, 32, 128],
            raw: None,
        };
        let list = |v: &str| v.split(',').map(|n| n.trim().parse().expect("a number")).collect();
        for pair in args.chunks(2) {
            let v = pair.get(1).map_or("", String::as_str);
            match pair[0].as_str() {
                "--image" => o.image = v.into(),
                "--seconds" => o.seconds = v.parse().expect("--seconds takes a number"),
                "--rates" => o.rates = list(v),
                "--p99-ms" => o.p99 = Duration::from_millis(v.parse().expect("a number")),
                "--alive" => o.alive = v.parse().expect("--alive takes a number"),
                "--cells" => o.cells = v.parse().expect("--cells takes a number"),
                "--in-flight" => {
                    o.in_flight = list(v).into_iter().map(|n: u32| n as usize).collect()
                }
                "--raw" => o.raw = Some(v.into()),
                other => panic!("unknown option {other}"),
            }
        }
        o
    }
}

async fn run(suite: &str, o: &Opts) -> Result<(), hive_sdk::Error> {
    let endpoint = std::env::var("HIVEBOX_ENDPOINT")
        .unwrap_or_else(|_| "unix:/run/hivebox/comb.sock".to_string());
    let run = format!("{}-{}", suite, std::process::id());
    let client = Client::connect(&endpoint).await?.project("bench")?;
    let t = Target { client, image: o.image.clone(), run };
    println!("{}", machine());
    println!("endpoint {endpoint}, image {}, run {}", o.image, t.run);
    println!();
    let mut raw = match &o.raw {
        Some(path) => Some(Raw::create(path).map_err(|e| {
            hive_sdk::Error::new(
                hive_sdk::Reason::InvalidArgument,
                format!("{}: {e}", path.display()),
            )
        })?),
        None => None,
    };
    let r = match suite {
        "create-storm" => storm(&t, o, &mut raw).await,
        _ => exec(&t, o, &mut raw).await,
    };
    // Whatever happened, leave nothing behind.
    let (left, took) = t.stop_all().await?;
    if left > 0 {
        eprintln!("stopped {left} cells left from the run in {} ms", node::ms(took));
    }
    r
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
    for &rate in &o.rates {
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
