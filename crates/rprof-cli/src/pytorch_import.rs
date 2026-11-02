use anyhow::{Result, ensure};
use clap::Args;
use std::{fs::File, path::PathBuf};
use uuid::Uuid;

#[derive(Args)]
pub struct ImportArgs {
    input: PathBuf,
    #[arg(short, long)]
    output: PathBuf,
    /// Optional shared capture session; source clocks still remain separate.
    #[arg(long)]
    session: Option<Uuid>,
}
pub fn run(args: ImportArgs) -> Result<()> {
    ensure!(
        !args.output.exists(),
        "output {} already exists",
        args.output.display()
    );
    let temporary = tempfile::tempdir()?;
    let converted = temporary.path().join("import.jsonl");
    let summary = rprof_pytorch_import::import(&args.input, &converted, args.session)?;
    crate::report::write_artifact(&args.output, |out| {
        std::io::copy(&mut File::open(&converted)?, out)?;
        Ok(())
    })?;
    eprintln!(
        "PyTorch import: {} records, {} timed spans, {} skipped, {} unresolved flows, {} writer losses",
        summary.source_records,
        summary.spans,
        summary.skipped_records,
        summary.unresolved_flows,
        summary.writer_loss
    );
    Ok(())
}
