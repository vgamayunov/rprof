use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rprof_core::stream::{Header, Record};
use uuid::Uuid;

use crate::{MAX_FRAME_BYTES, MAX_SHARDS, MAX_TRACKS, Reader, Summary, summarize, writer::encode};

/// Deterministic, bounded-memory external merge. IDs are the composite
/// (session, shard UUID, local ID); numeric IDs never collide across namespaces.
/// Spool files avoid retaining events or opening one descriptor per shard.
/// Frames are sorted by shard UUID, preserving validated per-shard sequence.
/// No cross-host timestamp alignment is inferred from wall clocks.
pub fn merge(inputs: &[PathBuf], output: &Path) -> Result<Summary> {
    ensure!(!inputs.is_empty(), "at least one input capture is required");
    ensure!(inputs.len() <= MAX_SHARDS, "too many input captures");
    ensure!(
        !output.exists(),
        "output {} already exists",
        output.display()
    );
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let spool = tempfile::Builder::new()
        .prefix(".rprof-merge-")
        .tempdir_in(parent)?;
    let mut shards = BTreeMap::<Uuid, PathBuf>::new();
    let mut session = None;
    let mut recovered_inputs = 0u64;
    let mut tracks = 0u64;
    let mut metadata_bytes = 0usize;
    for input in inputs {
        let mut reader = Reader::new(BufReader::new(
            File::open(input).with_context(|| format!("opening {}", input.display()))?,
        ));
        while let Some(record) = reader
            .next_record()
            .with_context(|| format!("reading {}", input.display()))?
        {
            let id = match &record {
                Record::Header(header) => {
                    if let Some(session) = session {
                        ensure!(
                            header.session_id == session,
                            "cannot merge unrelated sessions"
                        );
                    } else {
                        session = Some(header.session_id);
                    }
                    recovered_inputs = recovered_inputs
                        .checked_add(header.recovered_inputs)
                        .context("recovery count overflow")?;
                    continue;
                }
                Record::Shard(shard) => {
                    ensure!(
                        !shards.contains_key(&shard.shard_id),
                        "duplicate shard {} across inputs",
                        shard.shard_id
                    );
                    ensure!(shards.len() < MAX_SHARDS, "merged shard limit exceeded");
                    let path = spool.path().join(shard.shard_id.to_string());
                    File::create_new(&path)?;
                    shards.insert(shard.shard_id, path);
                    metadata_bytes += encode(&record, MAX_FRAME_BYTES)?.len();
                    ensure!(
                        metadata_bytes <= 16 * 1024 * 1024,
                        "merged metadata exceeds 16 MiB"
                    );
                    shard.shard_id
                }
                Record::Chunk(chunk) => chunk.shard_id,
                Record::End(end) => end.shard_id,
            };
            // Only one spool descriptor is open at a time, even for thousands of ranks.
            OpenOptions::new()
                .append(true)
                .open(shards.get(&id).context("undeclared shard")?)?
                .write_all(&encode(&record, MAX_FRAME_BYTES)?)?;
        }
        let summary = reader.summary()?;
        ensure!(
            !summary.shards.is_empty(),
            "input {} contains no shards",
            input.display()
        );
        recovered_inputs = recovered_inputs
            .checked_add(u64::from(summary.truncated_tail))
            .context("recovery count overflow")?;
        tracks += summary.shards.values().map(|s| s.tracks).sum::<u64>();
        ensure!(tracks <= MAX_TRACKS as u64, "merged track limit exceeded");
    }
    let mut header = Header::new(session.context("no input session")?);
    header.recovered_inputs = recovered_inputs;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&encode(&Record::Header(header), MAX_FRAME_BYTES)?)?;
    for path in shards.values() {
        io::copy(&mut File::open(path)?, &mut file)?;
    }
    file.flush()?;
    let summary = summarize(BufReader::new(File::open(file.path())?))?;
    file.as_file().sync_all()?;
    file.persist_noclobber(output)
        .with_context(|| format!("publishing {}", output.display()))?;
    Ok(summary)
}
