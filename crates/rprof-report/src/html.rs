use std::io::Write;

use anyhow::{Context, Result};

use crate::Report;

const TEMPLATE: &str = include_str!("../../../ui/index.html");
const SCRIPT: &str = include_str!("../../../ui/dist/viewer.js");
const STYLE: &str = include_str!("../../../ui/dist/viewer.css");

/// Write an offline artifact. Data is never concatenated into executable code.
/// Escape '<' to prevent a workload string from terminating the inert script tag;
/// also escape '&' and '>'. Unicode line separators are valid in inert JSON.
pub fn write_html(report: &Report, mut out: impl Write) -> Result<()> {
    let template = TEMPLATE
        .replacen("{{STYLE}}", STYLE, 1)
        .replacen("{{SCRIPT}}", SCRIPT, 1);
    let (before, after) = template
        .split_once("{{DATA}}")
        .context("HTML template missing data placeholder")?;
    out.write_all(before.as_bytes())?;
    struct Escaped<W>(W);
    impl<W: Write> Write for Escaped<W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            // JSON serializes UTF-8 string fragments; escaping byte sequences for
            // '<', '>', '&' is sufficient for tag safety. Line separators are valid
            // in inert JSON and need no JavaScript string-literal interpretation.
            let mut start = 0;
            for (index, byte) in bytes.iter().enumerate() {
                let escaped: &[u8] = match byte {
                    b'<' => b"\\u003c",
                    b'>' => b"\\u003e",
                    b'&' => b"\\u0026",
                    _ => continue,
                };
                self.0.write_all(&bytes[start..index])?;
                self.0.write_all(escaped)?;
                start = index + 1;
            }
            self.0.write_all(&bytes[start..])?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }
    serde_json::to_writer(Escaped(&mut out), report)?;
    out.write_all(after.as_bytes())?;
    Ok(())
}
