use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use rprof_report::{Limits, analyze, write_html, write_text, write_trace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Text,
    Html,
    #[value(alias = "chrome", alias = "perfetto")]
    Trace,
}

#[derive(Args)]
pub struct ReportArgs {
    pub input: PathBuf,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Output artifact; otherwise write to stdout. Existing files are never replaced.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Clock domain to export for --format trace (list IDs in the text report).
    #[arg(long)]
    domain: Option<String>,
    #[command(flatten)]
    pub limits: TimelineLimits,
}

#[derive(Args)]
pub struct TimelineLimits {
    /// Raw observations per standalone report or indexed window (indexed server cap: 20000).
    #[arg(long, default_value_t = 200_000, value_parser = clap::value_parser!(u32).range(0..=2_000_000))]
    max_events: u32,
    /// Raw retention budget in MiB; indexed replies have a separate 16 MiB server cap.
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=512))]
    max_report_mib: u32,
}

impl TimelineLimits {
    pub fn options(&self, retain_timeline: bool) -> Limits {
        Limits {
            retain_timeline,
            max_events: self.max_events as usize,
            max_bytes: self.max_report_mib as usize * 1024 * 1024,
        }
    }
}

pub fn report(args: ReportArgs) -> Result<()> {
    ensure!(
        args.domain.is_none() || args.format == Format::Trace,
        "--domain applies to --format trace; HTML has a domain selector"
    );
    let render = |out: &mut dyn Write| -> Result<()> {
        match args.format {
            Format::Trace => write_trace(&args.input, out, args.domain.as_deref()),
            Format::Text | Format::Html => {
                let report = analyze(
                    &args.input,
                    args.limits.options(args.format == Format::Html),
                )?;
                if args.format == Format::Html {
                    write_html(&report, out)
                } else {
                    write_text(&report, out)
                }
            }
        }
    };
    if let Some(path) = args.output {
        write_artifact(&path, render)?;
        eprintln!("Report written to {}", path.display());
    } else {
        let mut out = BufWriter::new(std::io::stdout().lock());
        render(&mut out)?;
        out.flush()?;
    }
    Ok(())
}

pub fn write_artifact(
    path: &Path,
    render: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    ensure!(!path.exists(), "output {} already exists", path.display());
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating report in {}", parent.display()))?;
    {
        let mut out = BufWriter::new(file.as_file_mut());
        render(&mut out)?;
        out.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .with_context(|| format!("publishing {}", path.display()))?;
    Ok(())
}
