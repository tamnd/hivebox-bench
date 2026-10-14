//! The raw results of a run: one JSON line per call, compressed with zstd.
//!
//! The tables in a report are computed from these samples, so someone who doubts a table can
//! compute it again, or compute something the report did not. A line looks like this:
//!
//! ```text
//! {"suite":"exec","step":"32","at_us":1204,"took_us":6931,"error":null}
//! ```
//!
//! `at_us` is when the call started, in microseconds from the start of its step, and `took_us` is
//! how long it took to answer. [`read`] reads a file back and [`table`] prints its tables again.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use crate::node::{Sample, ms};
use crate::stats::Summary;

/// A raw results file being written. The zstd frame is finished when it is dropped.
pub struct Raw {
    out: zstd::stream::AutoFinishEncoder<'static, BufWriter<File>>,
}

impl std::fmt::Debug for Raw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Raw").finish_non_exhaustive()
    }
}

impl Raw {
    /// Creates `path`, replacing what was there.
    ///
    /// # Errors
    ///
    /// The file could not be created.
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let file = BufWriter::new(File::create(path)?);
        Ok(Self { out: zstd::Encoder::new(file, 3)?.auto_finish() })
    }

    /// Writes one line per sample, all under `suite` and `step`.
    ///
    /// # Errors
    ///
    /// The write failed.
    pub fn write(&mut self, suite: &str, step: &str, samples: &[Sample]) -> std::io::Result<()> {
        for s in samples {
            writeln!(self.out, "{}", line(suite, step, s))?;
        }
        Ok(())
    }
}

fn line(suite: &str, step: &str, s: &Sample) -> String {
    let error = s.error.as_deref().map_or_else(|| "null".to_string(), quote);
    format!(
        r#"{{"suite":{},"step":{},"at_us":{},"took_us":{},"error":{error}}}"#,
        quote(suite),
        quote(step),
        s.at.as_micros(),
        s.took.as_micros()
    )
}

/// One line of a raw results file, read back.
#[derive(Clone, Debug)]
pub struct Line {
    /// The suite that made the call.
    pub suite: String,
    /// The step it was in, a rate or a number in flight.
    pub step: String,
    /// The call.
    pub sample: Sample,
}

/// Reads every line of the raw results file at `path`.
///
/// # Errors
///
/// The file cannot be read, is not zstd, or has a line this module did not write.
pub fn read(path: &Path) -> std::io::Result<Vec<Line>> {
    let text = BufReader::new(zstd::Decoder::new(File::open(path)?)?);
    let mut lines = Vec::new();
    for (n, l) in text.lines().enumerate() {
        let l = l?;
        let line = parse(&l).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line {} is not a raw result: {l}", n + 1),
            )
        })?;
        lines.push(line);
    }
    Ok(lines)
}

/// The markdown table for `lines`, a row for each suite and step in the order they first come.
/// Calls a second is the calls that succeeded over the time from the start of the step to the
/// last answer, which is a little shorter than the wall time the run itself printed.
#[must_use]
pub fn table(lines: &[Line]) -> String {
    let mut steps: Vec<(&str, &str, Vec<&Sample>)> = Vec::new();
    for l in lines {
        match steps.iter_mut().find(|(su, st, _)| *su == l.suite && *st == l.step) {
            Some((_, _, v)) => v.push(&l.sample),
            None => steps.push((l.suite.as_str(), l.step.as_str(), vec![&l.sample])),
        }
    }
    let mut s = String::from(
        "| suite | step | calls | failed | per second | p50 ms | p90 ms | p99 ms | max ms | iqr ms |\n",
    );
    s.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for (suite, step, samples) in steps {
        let ok: Vec<Duration> =
            samples.iter().filter(|x| x.error.is_none()).map(|x| x.took).collect();
        let span = samples.iter().map(|x| x.at + x.took).max().unwrap_or_default();
        let rate = if span.is_zero() { 0.0 } else { ok.len() as f64 / span.as_secs_f64() };
        let failed = samples.len() - ok.len();
        let lat = Summary::of(&ok).map_or_else(
            || "| | | | |".to_string(),
            |l| {
                format!(
                    "{} | {} | {} | {} | {}",
                    ms(l.p50),
                    ms(l.p90),
                    ms(l.p99),
                    ms(l.max),
                    ms(l.iqr)
                )
            },
        );
        s.push_str(&format!(
            "| {suite} | {step} | {} | {failed} | {rate:.1} | {lat} |\n",
            samples.len()
        ));
    }
    s
}

/// One line as [`line`] writes it, or `None` when it is anything else.
fn parse(text: &str) -> Option<Line> {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    p.eat(b'{')?;
    let (mut suite, mut step, mut at, mut took, mut error) = (None, None, None, None, None);
    loop {
        let key = p.string()?;
        p.eat(b':')?;
        match key.as_str() {
            "suite" => suite = Some(p.string()?),
            "step" => step = Some(p.string()?),
            "at_us" => at = Some(Duration::from_micros(p.number()?)),
            "took_us" => took = Some(Duration::from_micros(p.number()?)),
            "error" if p.literal(b"null") => error = Some(None),
            "error" => error = Some(Some(p.string()?)),
            _ => return None,
        }
        if p.eat(b'}').is_some() {
            break;
        }
        p.eat(b',')?;
    }
    (p.i == p.s.len()).then_some(())?;
    Some(Line {
        suite: suite?,
        step: step?,
        sample: Sample { at: at?, took: took?, error: error? },
    })
}

/// Just enough of a JSON reader for the lines [`line`] writes.
struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn eat(&mut self, b: u8) -> Option<()> {
        (self.s.get(self.i) == Some(&b)).then(|| self.i += 1)
    }

    fn literal(&mut self, word: &[u8]) -> bool {
        let found = self.s[self.i..].starts_with(word);
        if found {
            self.i += word.len();
        }
        found
    }

    fn number(&mut self) -> Option<u64> {
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = Vec::new();
        loop {
            let b = *self.s.get(self.i)?;
            self.i += 1;
            match b {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hex = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            char::from_u32(u32::from_str_radix(hex, 16).ok()?)?
                        }
                        _ => return None,
                    };
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                b => out.push(b),
            }
        }
    }
}

/// `text` as a JSON string.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Duration;

    #[test]
    fn lines_are_json_and_come_back_out_of_zstd() {
        let path = std::env::temp_dir().join(format!("raw-{}.jsonl.zst", std::process::id()));
        let ok =
            Sample { at: Duration::from_micros(5), took: Duration::from_micros(6931), error: None };
        let bad = Sample { error: Some("said \"no\"\n\u{1}".into()), ..ok.clone() };
        {
            let mut raw = Raw::create(&path).unwrap();
            raw.write("exec", "32", &[ok, bad]).unwrap();
        }
        let mut text = String::new();
        zstd::Decoder::new(File::open(&path).unwrap()).unwrap().read_to_string(&mut text).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            text,
            concat!(
                r#"{"suite":"exec","step":"32","at_us":5,"took_us":6931,"error":null}"#,
                "\n",
                r#"{"suite":"exec","step":"32","at_us":5,"took_us":6931,"error":"said \"no\"\n\u0001"}"#,
                "\n"
            )
        );
    }

    #[test]
    fn lines_read_back_as_they_were_written() {
        let ok =
            Sample { at: Duration::from_micros(5), took: Duration::from_micros(6931), error: None };
        let bad = Sample { error: Some("said \"no\"\n\u{1} \u{e9}".into()), ..ok.clone() };
        for s in [ok, bad] {
            let back = parse(&line("exec", "32", &s)).unwrap();
            assert_eq!((back.suite.as_str(), back.step.as_str()), ("exec", "32"));
            assert_eq!((back.sample.at, back.sample.took), (s.at, s.took));
            assert_eq!(back.sample.error, s.error);
        }
        assert!(parse(r#"{"suite":"exec","step":"1","at_us":1,"took_us":2}"#).is_none());
        assert!(
            parse(r#"{"suite":"exec","step":"1","at_us":1,"took_us":2,"error":null}x"#).is_none()
        );
    }

    #[test]
    fn the_table_has_a_row_per_step_in_order() {
        let at = |a, t, e: Option<&str>| Line {
            suite: "exec".into(),
            step: if a < 100 { "1".into() } else { "8".into() },
            sample: Sample {
                at: Duration::from_millis(a),
                took: Duration::from_millis(t),
                error: e.map(str::to_string),
            },
        };
        let lines = [at(0, 10, None), at(100, 50, None), at(10, 990, None), at(200, 5, Some("no"))];
        let t = table(&lines);
        let rows: Vec<&str> = t.lines().skip(2).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].starts_with("| exec | 1 | 2 | 0 | 2.0 | 10.0 |"), "{}", rows[0]);
        assert!(rows[1].starts_with("| exec | 8 | 2 | 1 | 4.9 | 50.0 |"), "{}", rows[1]);
    }
}
