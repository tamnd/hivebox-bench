//! The Docker baseline for create-storm and exec: the same steps, on the same machine, against the
//! Docker Engine API socket, the way a platform built on Docker makes and uses its sandboxes.
//!
//! A container here stands in for a cell: made from the same root filesystem, held to the same
//! memory limit, with loopback only for its network (`NetworkMode` none, which is what a cell has
//! with the guard off), and kept alive by `sleep infinity` until it is removed. A create is the
//! create call and the start call, since a cell is running when its create answers. An exec is
//! the three calls the docker CLI makes for `docker exec`: make the exec, start it and read its
//! output until it ends, and ask for its exit code. Each call goes on a connection of its own, as
//! the CLI's do.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Semaphore;

use crate::node::{ExecStep, Sample, StormStep};
use crate::stats::Summary;

/// The label every container of a run carries, with the run's id, so the run removes its own
/// containers and nobody else's.
pub const LABEL: &str = "hivebox-bench";

/// How many creates `fill` keeps in flight, and how many removes `stop_all` does.
const PARALLEL: usize = 16;

/// A call that failed, and whether it was Docker's fault and not the caller's.
#[derive(Clone, Debug)]
pub struct Failure {
    /// What went wrong.
    pub msg: String,
    /// A 5xx answer or a broken connection, not a 4xx one.
    pub infra: bool,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

fn io(socket: &std::path::Path, e: &std::io::Error) -> Failure {
    Failure { msg: format!("{}: {e}", socket.display()), infra: true }
}

/// Where the containers come from and how they are labelled.
#[derive(Clone, Debug)]
pub struct Docker {
    /// The Engine API socket, usually `/var/run/docker.sock`.
    pub socket: PathBuf,
    /// The image every container starts from.
    pub image: String,
    /// The value of the `hivebox-bench` label on every container of this run.
    pub run: String,
    /// The memory limit of each container in MiB, or none.
    pub mem_mib: Option<u32>,
}

impl Docker {
    /// One call to the Engine API. Returns the status and the body.
    ///
    /// # Errors
    ///
    /// The socket could not be reached or the answer was not HTTP.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String), Failure> {
        let mut s = UnixStream::connect(&self.socket).await.map_err(|e| io(&self.socket, &e))?;
        let body = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: docker\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.map_err(|e| io(&self.socket, &e))?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.map_err(|e| io(&self.socket, &e))?;
        parse(&buf).ok_or_else(|| Failure {
            msg: format!("{method} {path}: not an HTTP answer"),
            infra: true,
        })
    }

    /// A call that must answer with `want`.
    async fn expect(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        want: u16,
    ) -> Result<String, Failure> {
        let (status, b) = self.call(method, path, body).await?;
        if status == want {
            Ok(b)
        } else {
            let what = path.split('/').rfind(|p| !p.is_empty() && p.len() < 20);
            Err(Failure {
                msg: format!("{method} {}: {status} {}", what.unwrap_or(path), b.trim()),
                infra: status >= 500,
            })
        }
    }

    /// The daemon's version.
    ///
    /// # Errors
    ///
    /// Docker is not there.
    pub async fn version(&self) -> Result<String, Failure> {
        let b = self.expect("GET", "/version", None, 200).await?;
        field(&b, "Version")
            .ok_or_else(|| Failure { msg: format!("no version in {b}"), infra: true })
    }

    /// Makes and starts one container labelled with the run and `step`, and returns its id.
    ///
    /// # Errors
    ///
    /// Docker would not make or start it.
    pub async fn create(&self, step: &str) -> Result<String, Failure> {
        let mem = self
            .mem_mib
            .map_or_else(String::new, |m| format!(",\"Memory\":{}", u64::from(m) << 20));
        let body = format!(
            "{{\"Image\":{},\"Cmd\":[\"sleep\",\"infinity\"],\"Labels\":{{\"{LABEL}\":{},\"step\":{}}},\"HostConfig\":{{\"NetworkMode\":\"none\"{mem}}}}}",
            quote(&self.image),
            quote(&self.run),
            quote(step)
        );
        let b = self.expect("POST", "/containers/create", Some(&body), 201).await?;
        let id =
            field(&b, "Id").ok_or_else(|| Failure { msg: format!("no id in {b}"), infra: true })?;
        self.expect("POST", &format!("/containers/{id}/start"), None, 204).await?;
        Ok(id)
    }

    /// Runs `argv` in container `id` the way `docker exec` does, and returns its exit code.
    ///
    /// # Errors
    ///
    /// One of the three calls failed, or the exit code was not there.
    pub async fn exec(&self, id: &str, argv: &[&str]) -> Result<i64, Failure> {
        let cmd: Vec<String> = argv.iter().map(|a| quote(a)).collect();
        let body =
            format!("{{\"Cmd\":[{}],\"AttachStdout\":true,\"AttachStderr\":true}}", cmd.join(","));
        let b = self.expect("POST", &format!("/containers/{id}/exec"), Some(&body), 201).await?;
        let exec =
            field(&b, "Id").ok_or_else(|| Failure { msg: format!("no id in {b}"), infra: true })?;
        let start = "{\"Detach\":false,\"Tty\":false}";
        self.expect("POST", &format!("/exec/{exec}/start"), Some(start), 200).await?;
        let b = self.expect("GET", &format!("/exec/{exec}/json"), None, 200).await?;
        number(&b, "ExitCode")
            .ok_or_else(|| Failure { msg: format!("no exit code in {b}"), infra: true })
    }

    /// Removes every container of this run, `PARALLEL` at a time since Docker has no call that
    /// removes many, and returns how many there were and how long it took.
    ///
    /// # Errors
    ///
    /// The containers could not be listed, or one could not be removed.
    ///
    /// # Panics
    ///
    /// A remove task panicked.
    pub async fn stop_all(&self) -> Result<(u32, Duration), Failure> {
        let t = Instant::now();
        // {"label":["hivebox-bench=<run>"]}, encoded for the query string.
        let filter = format!("%7B%22label%22%3A%5B%22{LABEL}%3D{}%22%5D%7D", self.run);
        let b = self
            .expect("GET", &format!("/containers/json?all=1&filters={filter}"), None, 200)
            .await?;
        let ids = all_fields(&b, "Id");
        let gate = Arc::new(Semaphore::new(PARALLEL));
        let mut tasks = Vec::new();
        for id in &ids {
            let (d, id, gate) = (self.clone(), id.clone(), gate.clone());
            tasks.push(tokio::spawn(async move {
                let _permit = gate.acquire_owned().await;
                d.expect("DELETE", &format!("/containers/{id}?force=true"), None, 204).await
            }));
        }
        let mut first = None;
        for task in tasks {
            if let Err(e) = task.await.expect("a remove task panicked") {
                first.get_or_insert(e);
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok((u32::try_from(ids.len()).unwrap_or(u32::MAX), t.elapsed())),
        }
    }
}

/// Starts `rate` creates a second for `seconds`, open loop, and removes every container at the
/// end of the step, as `node::storm_step` does with cells.
///
/// # Errors
///
/// Removing the containers failed.
///
/// # Panics
///
/// A create task panicked.
pub async fn storm_step(d: &Docker, rate: u32, seconds: u32) -> Result<StormStep, Failure> {
    let step = Arc::new(format!("storm-{rate}"));
    let total = (rate * seconds) as usize;
    let gap = Duration::from_secs(1) / rate.max(1);
    let start = tokio::time::Instant::now();
    let mut tasks = Vec::with_capacity(total);
    for i in 0..total {
        tokio::time::sleep_until(start + gap * i as u32).await;
        let (d, step) = (d.clone(), step.clone());
        let at = start.elapsed();
        tasks.push(tokio::spawn(async move {
            let t = Instant::now();
            let r = d.create(&step).await;
            (at, t.elapsed(), r)
        }));
    }
    let (mut ok, mut failed, mut infra, mut first_error) = (Vec::new(), 0, 0, None);
    let mut samples = Vec::with_capacity(total);
    for task in tasks {
        let (at, took, r) = task.await.expect("a create task panicked");
        let error = match r {
            Ok(_) => {
                ok.push(took);
                None
            }
            Err(e) => {
                failed += 1;
                infra += usize::from(e.infra);
                first_error.get_or_insert_with(|| e.msg.clone());
                Some(e.msg)
            }
        };
        samples.push(Sample { at, took, error });
    }
    let wall = start.elapsed();
    let stopped = d.stop_all().await?;
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

/// Makes `n` containers, `PARALLEL` creates in flight, and keeps them. Returns their ids, how
/// long it took and the failures.
///
/// # Panics
///
/// A create task panicked.
pub async fn fill(d: &Docker, step: &str, n: u32) -> (Vec<String>, Duration, Vec<Failure>) {
    let started = Instant::now();
    let gate = Arc::new(Semaphore::new(PARALLEL));
    let mut tasks = Vec::new();
    for _ in 0..n {
        let (d, step, gate) = (d.clone(), step.to_string(), gate.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = gate.acquire_owned().await;
            d.create(&step).await
        }));
    }
    let (mut ids, mut errors) = (Vec::new(), Vec::new());
    for task in tasks {
        match task.await.expect("a create task panicked") {
            Ok(id) => ids.push(id),
            Err(e) => errors.push(e),
        }
    }
    (ids, started.elapsed(), errors)
}

/// Runs `argv` in the containers for `seconds`, `concurrency` at a time, closed loop, each worker
/// going round them from its own starting point, as `node::exec_step` does with cells.
///
/// # Panics
///
/// A worker panicked.
pub async fn exec_step(
    d: &Docker,
    ids: &[String],
    argv: &'static [&'static str],
    concurrency: usize,
    seconds: u32,
) -> ExecStep {
    let ids: Arc<Vec<String>> = Arc::new(ids.to_vec());
    let end = Instant::now() + Duration::from_secs(u64::from(seconds));
    let start = Instant::now();
    let mut workers = Vec::new();
    for w in 0..concurrency {
        let (d, ids) = (d.clone(), ids.clone());
        workers.push(tokio::spawn(async move {
            let (mut ok, mut failed, mut first, mut samples) =
                (Vec::new(), 0usize, None, Vec::new());
            let mut i = w;
            while Instant::now() < end {
                let id = &ids[i % ids.len()];
                i += concurrency;
                let t = Instant::now();
                let r = d.exec(id, argv).await;
                let took = t.elapsed();
                let error = match r {
                    Ok(0) => {
                        ok.push(took);
                        None
                    }
                    Ok(code) => Some(format!("exit {code}")),
                    Err(e) => Some(e.msg),
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
        cells: ids.len(),
        ran: ok.len() + failed,
        failed,
        first_error,
        wall,
        latency: Summary::of(&ok),
        samples,
    }
}

/// The status and body of an HTTP/1.1 answer read to its end, with a chunked body put back
/// together.
fn parse(buf: &[u8]) -> Option<(u16, String)> {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..split]);
    let mut body = &buf[split + 4..];
    let status = head.lines().next()?.split(' ').nth(1)?.parse().ok()?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if !chunked {
        return Some((status, String::from_utf8_lossy(body).into_owned()));
    }
    let mut out = Vec::new();
    loop {
        let end = body.windows(2).position(|w| w == b"\r\n")?;
        let size = String::from_utf8_lossy(&body[..end]);
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        body = &body[end + 2..];
        if size == 0 {
            break;
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
    Some((status, String::from_utf8_lossy(&out).into_owned()))
}

/// Every string value of `key` in a JSON text, in order. Docker's answers are compact, and the
/// values read here are ids and versions, which have nothing to escape.
fn all_fields(json: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\":\"");
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find(&pat) {
        rest = &rest[i + pat.len()..];
        let Some(end) = rest.find('"') else { break };
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

fn field(json: &str, key: &str) -> Option<String> {
    all_fields(json, key).into_iter().next()
}

/// The integer value of `key`, and none when it is `null` or missing.
fn number(json: &str, key: &str) -> Option<i64> {
    let pat = format!("\"{key}\":");
    let rest = &json[json.find(&pat)? + pat.len()..];
    let end = rest.find(|c: char| c != '-' && !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `s` as a JSON string.
fn quote(s: &str) -> String {
    let mut q = String::with_capacity(s.len() + 2);
    q.push('"');
    for c in s.chars() {
        match c {
            '"' => q.push_str("\\\""),
            '\\' => q.push_str("\\\\"),
            c if u32::from(c) < 0x20 => q.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_answer_is_read() {
        let r = parse(b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\n\r\n{\"Id\":\"ab12\",\"Warnings\":[]}\n");
        assert_eq!(r, Some((201, "{\"Id\":\"ab12\",\"Warnings\":[]}\n".to_string())));
        assert_eq!(parse(b"HTTP/1.1 204 No Content\r\n\r\n"), Some((204, String::new())));
        assert_eq!(parse(b"garbage"), None);
    }

    #[test]
    fn a_chunked_answer_is_put_back_together() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n[{\"Id\"\r\nA;x=1\r\n:\"c1\"},{\"I\r\n9\r\nd\":\"c2\"}]\r\n0\r\n\r\n";
        let (status, body) = parse(raw).expect("an answer");
        assert_eq!(status, 200);
        assert_eq!(all_fields(&body, "Id"), ["c1", "c2"]);
        assert_eq!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nab"), None);
    }

    #[test]
    fn exit_codes_and_strings() {
        assert_eq!(number("{\"ExitCode\":0,\"Running\":false}", "ExitCode"), Some(0));
        assert_eq!(number("{\"ExitCode\":-1}", "ExitCode"), Some(-1));
        assert_eq!(number("{\"ExitCode\":null}", "ExitCode"), None);
        assert_eq!(field("{\"Version\":\"29.6.2\"}", "Version").as_deref(), Some("29.6.2"));
        assert_eq!(quote("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }
}
