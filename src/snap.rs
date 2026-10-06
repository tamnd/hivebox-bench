//! The snapshot suite: pause and resume, and disk snapshots of cells with a known amount of
//! changed data, each one restored into a new cell and checked.
//!
//! Pause and resume are timed as the caller sees them, with a no-op command after each resume to
//! show the cell came back. A disk snapshot is taken of a fresh cell that has written `n` MiB of
//! random data, while a second task keeps running `true` in the same cell: the cell is frozen
//! while its changes are sealed, so the slowest of those commands is how long a user of the cell
//! waited. The snapshot is then restored into a new cell, whose checksum of the data has to match.
//!
//! Forking a running cell needs the microVM backend, which needs KVM, and is not in here.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hive_sdk::{Cell, Error, Reason, Source};

use crate::node::{Sample, Target, ms};
use crate::stats::Summary;

/// What a snapshot run does.
#[derive(Clone, Debug)]
pub struct SnapOpts {
    /// Pause and resume rounds on one cell.
    pub pauses: u32,
    /// The MiB each snapshotted cell writes before its snapshot.
    pub sizes: Vec<u32>,
    /// Snapshots per size.
    pub repeat: u32,
}

/// The pause and resume rounds.
#[derive(Clone, Debug, Default)]
pub struct Pauses {
    /// Every pause call.
    pub pause: Vec<Duration>,
    /// Every resume call.
    pub resume: Vec<Duration>,
    /// The `true` run right after each resume.
    pub after: Vec<Duration>,
}

/// The snapshots of one size.
#[derive(Clone, Debug)]
pub struct Size {
    /// MiB written before each snapshot.
    pub mib: u32,
    /// Every snapshot call.
    pub snapshot: Vec<Duration>,
    /// The slowest `true` that ran in the cell during each snapshot.
    pub stall: Vec<Duration>,
    /// How many `true` ran during each snapshot.
    pub probes: Vec<usize>,
    /// Every create from the snapshot.
    pub restore: Vec<Duration>,
    /// The first `true` in each restored cell, which is when it can be used.
    pub first: Vec<Duration>,
    /// Restores whose data did not match what was written.
    pub mismatched: u32,
}

/// Everything a snapshot run measured.
#[derive(Clone, Debug, Default)]
pub struct Results {
    /// A plain `true` in a cell with nothing else going on, the same command the probes run.
    pub idle: Vec<Duration>,
    /// The pause and resume rounds.
    pub pauses: Pauses,
    /// One entry per size, in the order of the options.
    pub sizes: Vec<Size>,
}

/// Runs the pause rounds and then every size.
///
/// # Errors
///
/// A cell could not be made, paused, resumed, written, snapshotted or restored.
pub async fn run(t: &Target, o: &SnapOpts) -> Result<Results, Error> {
    let mut r = Results::default();
    let cell = t.client.create(&t.spec("pause")).await?;
    for _ in 0..20 {
        r.idle.push(timed_true(&cell).await?);
    }
    for i in 1..=o.pauses {
        let at = Instant::now();
        cell.pause().await?;
        r.pauses.pause.push(at.elapsed());
        let at = Instant::now();
        cell.resume().await?;
        r.pauses.resume.push(at.elapsed());
        r.pauses.after.push(timed_true(&cell).await?);
        if i % 10 == 0 {
            eprintln!("pause round {i}");
        }
    }
    cell.stop().await?;
    for &mib in &o.sizes {
        let mut s = Size {
            mib,
            snapshot: Vec::new(),
            stall: Vec::new(),
            probes: Vec::new(),
            restore: Vec::new(),
            first: Vec::new(),
            mismatched: 0,
        };
        for i in 1..=o.repeat {
            one(t, &mut s).await?;
            eprintln!(
                "{mib} MiB #{i}: snapshot {} ms, slowest true {} ms, restore {} ms",
                ms(s.snapshot[s.snapshot.len() - 1]),
                ms(s.stall[s.stall.len() - 1]),
                ms(s.restore[s.restore.len() - 1]),
            );
        }
        r.sizes.push(s);
    }
    Ok(r)
}

/// One snapshot of `s.mib` MiB, restored and checked, with both cells stopped after.
async fn one(t: &Target, s: &mut Size) -> Result<(), Error> {
    let cell = t.client.create(&t.spec(&format!("snap-{}", s.mib))).await?;
    let write = format!(
        "head -c {}M /dev/urandom > /root/blob && sync && md5sum /root/blob | cut -d' ' -f1",
        s.mib
    );
    let sum = shell(&cell, &write).await?;
    let done = Arc::new(AtomicBool::new(false));
    let probe = {
        let (cell, done) = (cell.clone(), done.clone());
        tokio::spawn(async move {
            let mut took = Vec::new();
            while !done.load(Ordering::Relaxed) {
                took.push(timed_true(&cell).await?);
            }
            Ok::<_, Error>(took)
        })
    };
    // Let the probe get going so the freeze lands on a command already waiting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let at = Instant::now();
    let id = cell.snapshot(false, &[]).await;
    s.snapshot.push(at.elapsed());
    done.store(true, Ordering::Relaxed);
    let took = probe.await.map_err(|e| Error::new(Reason::Internal, e.to_string()))??;
    let id = id?;
    s.stall.push(took.iter().copied().max().unwrap_or_default());
    s.probes.push(took.len());
    cell.stop().await?;
    let mut spec = t.spec(&format!("restore-{}", s.mib));
    spec.source = Source::Snapshot(id);
    let at = Instant::now();
    let back = t.client.create(&spec).await?;
    s.restore.push(at.elapsed());
    s.first.push(timed_true(&back).await?);
    let got = shell(&back, "md5sum /root/blob | cut -d' ' -f1").await?;
    if got != sum {
        eprintln!("{} MiB: the restored blob is {got}, the written one was {sum}", s.mib);
        s.mismatched += 1;
    }
    back.stop().await
}

async fn timed_true(cell: &Cell) -> Result<Duration, Error> {
    let at = Instant::now();
    let r = cell.run(vec!["true".to_string()]).await?;
    let took = at.elapsed();
    if r.exit_code != 0 {
        return Err(Error::new(Reason::Internal, format!("true exited {}", r.exit_code)));
    }
    Ok(took)
}

/// Runs `script` with `sh -c` and returns its stdout trimmed, or fails on a non zero exit.
async fn shell(cell: &Cell, script: &str) -> Result<String, Error> {
    let r = cell.run(vec!["sh".to_string(), "-c".to_string(), script.to_string()]).await?;
    if r.exit_code != 0 {
        return Err(Error::new(
            Reason::Internal,
            format!("`{script}` exited {}: {}", r.exit_code, String::from_utf8_lossy(&r.stderr)),
        ));
    }
    Ok(String::from_utf8_lossy(&r.stdout).trim().to_string())
}

/// Samples for the raw results, one after the other.
#[must_use]
pub fn samples(took: &[Duration]) -> Vec<Sample> {
    let mut at = Duration::ZERO;
    took.iter()
        .map(|&took| {
            let s = Sample { at, took, error: None };
            at += took;
            s
        })
        .collect()
}

/// The markdown tables: one row per call kind with its distribution, then one row per size.
#[must_use]
pub fn table(r: &Results) -> String {
    let mut s = String::from(
        "| call | count | p50 ms | p90 ms | p99 ms | max ms |\n|---|---|---|---|---|---|\n",
    );
    let mut row = |name: String, took: &[Duration]| {
        if let Some(x) = Summary::of(took) {
            let _ = writeln!(
                s,
                "| {name} | {} | {} | {} | {} | {} |",
                x.count,
                ms(x.p50),
                ms(x.p90),
                ms(x.p99),
                ms(x.max)
            );
        }
    };
    row("`true`, idle".into(), &r.idle);
    row("pause".into(), &r.pauses.pause);
    row("resume".into(), &r.pauses.resume);
    row("`true` after resume".into(), &r.pauses.after);
    for z in &r.sizes {
        row(format!("snapshot, {} MiB", z.mib), &z.snapshot);
        row(format!("slowest `true` during it, {} MiB", z.mib), &z.stall);
        row(format!("restore, {} MiB", z.mib), &z.restore);
        row(format!("first `true` after restore, {} MiB", z.mib), &z.first);
    }
    s.push('\n');
    s.push_str("| MiB | snapshots | `true` during each | restores checked | mismatched |\n");
    s.push_str("|---|---|---|---|---|\n");
    for z in &r.sizes {
        let probes: Vec<String> = z.probes.iter().map(ToString::to_string).collect();
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} |",
            z.mib,
            z.snapshot.len(),
            probes.join(", "),
            z.restore.len(),
            z.mismatched
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_a_row_per_call_and_per_size() {
        let d = |n| vec![Duration::from_millis(n)];
        let r = Results {
            idle: d(5),
            pauses: Pauses { pause: d(10), resume: d(11), after: d(6) },
            sizes: vec![Size {
                mib: 10,
                snapshot: d(200),
                stall: d(150),
                probes: vec![3],
                restore: d(300),
                first: d(7),
                mismatched: 0,
            }],
        };
        let t = table(&r);
        assert!(t.contains("| snapshot, 10 MiB | 1 | 200.0 |"), "{t}");
        assert!(t.contains("| 10 | 1 | 3 | 1 | 0 |"), "{t}");
        assert_eq!(t.lines().count(), 2 + 8 + 1 + 2 + 1);
    }

    #[test]
    fn samples_follow_each_other() {
        let s = samples(&[Duration::from_millis(3), Duration::from_millis(4)]);
        assert_eq!(s[1].at, Duration::from_millis(3));
    }
}
