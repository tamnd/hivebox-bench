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
//! how long it took to answer.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::node::Sample;

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
}
