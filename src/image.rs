//! The image suites: cold-image and image-import, both through the `hive-nectar` binary.
//!
//! The binary does what a comb does to an image, without the rest of the comb around it: an
//! import builds the image's EROFS layers and puts their blobs in a store, and a run mounts the
//! image the way a cell gets it and runs one command in it. Going through the binary keeps the
//! harness from linking the image code, and it means a number here is the same work a person
//! gets from typing the command.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::node::ms;
use crate::stats::Summary;

/// Where the images live and the binary that handles them.
#[derive(Clone, Debug)]
pub struct Nectar {
    /// The `hive-nectar` binary.
    pub bin: PathBuf,
    /// `--store DIR` or `--s3 URL`, as the binary takes them.
    pub store: Vec<String>,
    /// Where imports keep their built layers.
    pub work: PathBuf,
}

impl Nectar {
    fn run(&self, args: &[&str]) -> Result<(String, String, Duration), String> {
        let t = Instant::now();
        let out = Command::new(&self.bin)
            .args(args)
            .args(&self.store)
            .output()
            .map_err(|e| format!("{}: {e}", self.bin.display()))?;
        let took = t.elapsed();
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        );
        if !out.status.success() {
            return Err(format!("hive-nectar {}: {stderr}", args.join(" ")));
        }
        Ok((stdout, stderr, took))
    }

    /// Imports the OCI layout at `layout` and returns the image's id, what the binary said about
    /// it, and how long it took.
    ///
    /// # Errors
    ///
    /// The import failed.
    pub fn import(&self, layout: &Path) -> Result<(String, String, Duration), String> {
        let (layout, work) = (layout.to_string_lossy(), self.work.to_string_lossy());
        self.run(&["import-oci", &layout, "--work", &work])
    }
}

/// Parses a duration the way Rust's `{:.2?}` prints one, like `1.25s`, `310.40ms` or `12.00µs`.
#[must_use]
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?);
    let n: f64 = num.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "ms" => n / 1e3,
        "µs" | "us" => n / 1e6,
        "ns" => n / 1e9,
        _ => return None,
    };
    Some(Duration::from_secs_f64(secs))
}

/// The mount time and the command's time from what `hive-nectar run` prints:
/// `lazy: mounted in 1.20s, ran in 310.40ms, exit status: 0, 1.51s in all`.
#[must_use]
pub fn parse_run(line: &str) -> Option<(Duration, Duration, Duration)> {
    let line = line.lines().rev().find(|l| l.contains("mounted in"))?;
    let after = |key: &str| {
        let rest = &line[line.find(key)? + key.len()..];
        parse_duration(rest.split([',', ' ']).find(|w| !w.is_empty())?)
    };
    let all = line.rsplit(", ").next()?.strip_suffix(" in all")?;
    Some((after("mounted in ")?, after("ran in ")?, parse_duration(all)?))
}

/// One way of starting an image cold, and what every repetition of it took.
#[derive(Clone, Debug)]
pub struct ColdMode {
    /// What this row is.
    pub name: String,
    /// The mount, the command, and both, for every repetition.
    pub runs: Vec<(Duration, Duration, Duration)>,
    /// Repetitions that failed, with the first reason.
    pub failed: (usize, Option<String>),
}

impl ColdMode {
    fn summary(&self, f: fn(&(Duration, Duration, Duration)) -> Duration) -> Option<Summary> {
        Summary::of(&self.runs.iter().map(f).collect::<Vec<_>>())
    }
}

/// Runs `cmd` in the image `id` `repeat` times with `--mode mode`, each time with an empty cache
/// and an empty work directory under `scratch`, so nothing of the image is on the node when it
/// starts.
pub fn cold_runs(
    n: &Nectar,
    name: &str,
    id: &str,
    mode: &str,
    cmd: &str,
    repeat: usize,
    scratch: &Path,
) -> ColdMode {
    let mut m = ColdMode { name: name.to_string(), runs: Vec::new(), failed: (0, None) };
    for i in 0..repeat {
        let dir = scratch.join(format!("{name}-{i}"));
        let (cache, work) = (dir.join("cache"), dir.join("work"));
        let made = std::fs::create_dir_all(&cache).and_then(|()| std::fs::create_dir_all(&work));
        let got = made.map_err(|e| e.to_string()).and_then(|()| {
            let (cache, work) = (cache.to_string_lossy(), work.to_string_lossy());
            let args =
                ["run", id, "--cache", &cache, "--work", &work, "--cmd", cmd, "--mode", mode];
            n.run(&args)
        });
        let _ = std::fs::remove_dir_all(&dir);
        let timed = got.and_then(|(_, err, _)| match parse_run(&err) {
            Some(t) if err.contains("exit status: 0") => Ok(t),
            _ => Err(format!("the command failed or gave no timings: {err}")),
        });
        match timed {
            Ok(t) => m.runs.push(t),
            Err(e) => {
                m.failed.0 += 1;
                m.failed.1.get_or_insert(e);
            }
        }
    }
    m
}

/// Runs `cmd` once in the image `id` with `--mode trace`, which stores what it read as the
/// image's prefetch traces, and returns the id of the traced image.
///
/// # Errors
///
/// The run failed or printed no image.
pub fn cold_runs_trace(n: &Nectar, id: &str, cmd: &str, scratch: &Path) -> Result<String, String> {
    let dir = scratch.join("trace");
    let (cache, work) = (dir.join("cache"), dir.join("work"));
    std::fs::create_dir_all(&cache)
        .and_then(|()| std::fs::create_dir_all(&work))
        .map_err(|e| e.to_string())?;
    let (c, w) = (cache.to_string_lossy(), work.to_string_lossy());
    let got = n.run(&["run", id, "--cache", &c, "--work", &w, "--cmd", cmd, "--mode", "trace"]);
    let _ = std::fs::remove_dir_all(&dir);
    let (out, err, _) = got?;
    match out.lines().last() {
        Some(traced) if err.contains("exit status: 0") => Ok(traced.trim().to_string()),
        _ => Err(format!("tracing gave no image: {err}")),
    }
}

/// The markdown table for cold-image.
#[must_use]
pub fn cold_table(modes: &[ColdMode]) -> String {
    let mut s = String::from(
        "| mode | runs | failed | mount p50 ms | mount p99 ms | command p50 ms | command p99 ms | total p50 ms | total p99 ms | total max ms |\n",
    );
    s.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    let cell =
        |x: Option<Summary>, f: fn(&Summary) -> Duration| x.map_or("-".into(), |x| ms(f(&x)));
    for m in modes {
        let (mount, run, all) = (m.summary(|r| r.0), m.summary(|r| r.1), m.summary(|r| r.2));
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            m.name,
            m.runs.len(),
            m.failed.0,
            cell(mount, |x| x.p50),
            cell(mount, |x| x.p99),
            cell(run, |x| x.p50),
            cell(run, |x| x.p99),
            cell(all, |x| x.p50),
            cell(all, |x| x.p99),
            cell(all, |x| x.max),
        );
    }
    s
}

/// The bytes under `dir`, counted from file sizes, and how many files there are.
#[must_use]
pub fn tree_size(dir: &Path) -> (u64, u64) {
    let (mut bytes, mut files) = (0, 0);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// One image put into the store.
#[derive(Clone, Debug)]
pub struct Imported {
    /// The layout's directory name.
    pub name: String,
    /// The compressed size of the layout's blobs, as a registry serves them.
    pub pulled: u64,
    /// The compressed size of the blobs no image before it had, which is what a node that keeps
    /// whole layers, as Docker does, would add.
    pub new_layers: u64,
    /// How much the store grew.
    pub grew: u64,
    /// How long the import took.
    pub took: Duration,
    /// What the binary said, or why it failed.
    pub said: Result<String, String>,
}

/// Imports every layout into one store in order, and measures how much each one adds to it. An
/// image that shares layers or chunks with one before it adds less than its size, which is what
/// the suite reports.
pub fn import_all(n: &Nectar, store_dir: &Path, layouts: &[PathBuf]) -> Vec<Imported> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for layout in layouts {
        let name = layout
            .file_name()
            .map_or_else(|| layout.display().to_string(), |f| f.to_string_lossy().into());
        let (mut pulled, mut new_layers) = (0, 0);
        if let Ok(blobs) = std::fs::read_dir(layout.join("blobs").join("sha256")) {
            for b in blobs.flatten() {
                let size = b.metadata().map_or(0, |m| m.len());
                pulled += size;
                if seen.insert(b.file_name()) {
                    new_layers += size;
                }
            }
        }
        let before = tree_size(store_dir).0;
        let t = Instant::now();
        let said = n.import(layout).map(|(id, err, _)| format!("{id} {err}"));
        let took = t.elapsed();
        let grew = tree_size(store_dir).0.saturating_sub(before);
        eprintln!(
            "{name}: {} MiB pulled, {} MiB stored, {:.1} s",
            pulled >> 20,
            grew >> 20,
            took.as_secs_f64()
        );
        out.push(Imported { name, pulled, new_layers, grew, took, said });
    }
    out
}

/// The markdown for image-import: one row per image and a total.
#[must_use]
pub fn import_table(rows: &[Imported], work: u64) -> String {
    let mib = |b: u64| format!("{:.1}", b as f64 / f64::from(1 << 20));
    let mut s = String::from(
        "| image | pulled MiB | new layers MiB | store grew MiB | pulled so far | layers so far | stored so far | import s |\n",
    );
    s.push_str("|---|---|---|---|---|---|---|---|\n");
    let (mut pulled, mut layers, mut stored, mut failed) = (0u64, 0u64, 0u64, Vec::new());
    for r in rows {
        if let Err(e) = &r.said {
            failed.push(format!("{}: {e}", r.name));
            continue;
        }
        pulled += r.pulled;
        layers += r.new_layers;
        stored += r.grew;
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {:.1} |",
            r.name,
            mib(r.pulled),
            mib(r.new_layers),
            mib(r.grew),
            mib(pulled),
            mib(layers),
            mib(stored),
            r.took.as_secs_f64()
        );
    }
    let ok = rows.len() - failed.len();
    let ratio = |a: u64, b: u64| a as f64 / b.max(1) as f64;
    let _ = writeln!(
        s,
        "\n{ok} images: {} MiB as pulled one by one, {} MiB as distinct compressed layers, {} MiB in the store. The store holds {:.2} times what the pulls did and {:.2} times the distinct layers, which are compressed. The import work directory holds another {} MiB of built layers that a node never reads.",
        mib(pulled),
        mib(layers),
        mib(stored),
        ratio(stored, pulled),
        ratio(stored, layers),
        mib(work)
    );
    for f in failed {
        let _ = writeln!(s, "\nfailed: {f}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_the_way_debug_prints_them() {
        assert_eq!(parse_duration("1.25s"), Some(Duration::from_millis(1250)));
        assert_eq!(parse_duration("310.40ms"), Some(Duration::from_micros(310_400)));
        assert_eq!(parse_duration("12.00µs"), Some(Duration::from_micros(12)));
        assert_eq!(parse_duration("3.00ns"), Some(Duration::from_nanos(3)));
        assert_eq!(parse_duration("fast"), None);
    }

    #[test]
    fn a_run_line_gives_mount_command_and_total() {
        let err =
            "some log line\nlazy: mounted in 1.20s, ran in 310.40ms, exit status: 0, 1.51s in all";
        let (mount, ran, all) = parse_run(err).unwrap();
        assert_eq!(mount, Duration::from_millis(1200));
        assert_eq!(ran, Duration::from_micros(310_400));
        assert_eq!(all, Duration::from_millis(1510));
        assert_eq!(parse_run("nothing here"), None);
    }
}
