use std::{
    fs::File,
    io::{self, BufWriter, Write},
    path::Path,
    sync::mpsc::{self, SyncSender, TrySendError},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, bail, ensure};
use rprof_core::{
    CounterValue,
    stream::{Attribute, CaptureConfig, Chunk, End, Event, Header, Loss, Payload, Record, Shard},
};
use serde::Serialize;
use uuid::Uuid;

use crate::MAX_FRAME_BYTES;

const MAX_EVENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub chunk_events: usize,
    pub queue_chunks: usize,
    pub flush_interval_ms: u64,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            chunk_events: 256,
            queue_chunks: 8,
            flush_interval_ms: 100,
        }
    }
}

impl RecorderConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=4096).contains(&self.chunk_events),
            "chunk-events must be 1..=4096"
        );
        ensure!(
            (1..=256).contains(&self.queue_chunks),
            "queue-chunks must be 1..=256"
        );
        ensure!(
            (1..=60_000).contains(&self.flush_interval_ms),
            "flush interval must be 1..=60000 ms"
        );
        Ok(())
    }

    pub fn metadata(&self) -> CaptureConfig {
        CaptureConfig {
            chunk_events: self.chunk_events,
            queue_chunks: self.queue_chunks,
            flush_interval_ms: self.flush_interval_ms,
        }
    }
}

/// Lifecycle: create (durable header), declare tracks reliably, emit observations,
/// periodically flush/checkpoint, finish (drain, footer, sync). Full queues drop
/// data batches, never structural declarations. This is the safe Rust staging
/// API; allocation-free per-thread native callback buffers come with collectors.
pub struct Recorder {
    shard_id: Uuid,
    config: RecorderConfig,
    pending: Vec<Event>,
    pending_bytes: usize,
    sequence: u64,
    loss: Loss,
    sender: Option<SyncSender<Vec<u8>>>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl Recorder {
    pub fn create(
        path: &Path,
        header: Header,
        shard: Shard,
        config: RecorderConfig,
    ) -> Result<Self> {
        config.validate()?;
        let header_frame = encode(&Record::Header(header), MAX_FRAME_BYTES)?;
        let shard_id = shard.shard_id;
        let shard_frame = encode(&Record::Shard(shard), MAX_FRAME_BYTES)?;
        let mut file = File::create_new(path)
            .with_context(|| format!("creating {} (must not already exist)", path.display()))?;
        file.write_all(&header_frame)?;
        file.write_all(&shard_frame)?;
        file.sync_data().context("persisting capture header")?;
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(config.queue_chunks);
        let worker = thread::Builder::new()
            .name("rprof-writer".into())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                for frame in receiver {
                    writer.write_all(&frame).context("writing capture frame")?;
                    writer.flush().context("flushing capture frame")?;
                }
                writer.flush()?;
                writer.get_ref().sync_data().context("persisting capture")
            })?;
        Ok(Self {
            shard_id,
            config,
            pending: Vec::new(),
            pending_bytes: 0,
            sequence: 0,
            loss: Loss::default(),
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    /// Nonblocking queue submission. Returns false if this event is oversized or
    /// its batch was dropped. A true result may still be in the staging batch.
    pub fn emit(&mut self, event: Event) -> Result<bool> {
        ensure!(
            !matches!(event.payload, Payload::Track(_)),
            "track declarations require reliable()"
        );
        validate_values(&event)?;
        let bytes = match encode(&event, MAX_EVENT_BYTES) {
            Ok(bytes) => bytes.len(),
            Err(_) => {
                self.loss.oversized = self.loss.oversized.saturating_add(1);
                return Ok(false);
            }
        };
        if self.pending_bytes + bytes > MAX_FRAME_BYTES - 4096 {
            self.flush()?;
        }
        self.pending_bytes += bytes;
        self.pending.push(event);
        if self.pending.len() >= self.config.chunk_events {
            return self.flush();
        }
        Ok(true)
    }

    /// Structural/start/end observations cannot be dropped. May block the caller;
    /// never call this from a native collector's hot callback path.
    pub fn reliable(&mut self, event: Event) -> Result<()> {
        validate_values(&event)?;
        self.flush_blocking()?;
        encode(&event, MAX_EVENT_BYTES).context("structural event exceeds size limit")?;
        self.pending.push(event);
        self.flush_blocking()
    }

    pub fn flush(&mut self) -> Result<bool> {
        self.submit(false)
    }

    fn flush_blocking(&mut self) -> Result<()> {
        self.submit(true).map(|_| ())
    }

    fn submit(&mut self, blocking: bool) -> Result<bool> {
        if self.pending.is_empty() {
            return Ok(true);
        }
        let count = self.pending.len() as u64;
        let chunk = Record::Chunk(Chunk {
            shard_id: self.shard_id,
            sequence: self.sequence,
            loss: self.loss.clone(),
            events: std::mem::take(&mut self.pending),
        });
        self.pending_bytes = 0;
        let frame = encode(&chunk, MAX_FRAME_BYTES)?;
        let sender = self.sender.as_ref().context("recorder is closed")?;
        if blocking {
            sender.send(frame).context("capture writer stopped")?;
        } else {
            match sender.try_send(frame) {
                Ok(()) => (),
                Err(TrySendError::Full(_)) => {
                    self.loss.queue_full = self.loss.queue_full.saturating_add(count);
                    return Ok(false);
                }
                Err(TrySendError::Disconnected(_)) => bail!("capture writer stopped"),
            }
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("chunk sequence overflow")?;
        Ok(true)
    }

    pub fn loss(&self) -> &Loss {
        &self.loss
    }

    pub fn finish(mut self, mut end: End) -> Result<()> {
        ensure!(
            end.shard_id == self.shard_id,
            "footer belongs to another shard"
        );
        self.flush_blocking()?;
        end.loss = self.loss.clone();
        self.sender
            .as_ref()
            .context("recorder is closed")?
            .send(encode(&Record::End(end), MAX_FRAME_BYTES)?)
            .context("capture writer stopped")?;
        self.sender.take();
        self.join()
    }

    fn join(&mut self) -> Result<()> {
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("capture writer panicked"))??;
        }
        Ok(())
    }
}

fn validate_values(event: &Event) -> Result<()> {
    for value in event.attributes.values() {
        if let Attribute::Float(value) = value {
            ensure!(value.is_finite(), "non-finite attribute");
        }
    }
    if let Payload::Counter(counter) = &event.payload {
        if let CounterValue::Float(value) = counter.value {
            ensure!(value.is_finite(), "non-finite counter");
        }
    }
    Ok(())
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.sender.take();
        let _ = self.join();
    }
}

/// The serializer itself is capped, not just its result.
pub(crate) fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    struct Capped {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Capped {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("frame size limit exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut out = Capped {
        bytes: Vec::new(),
        limit: limit.saturating_sub(1),
    };
    serde_json::to_writer(&mut out, value)?;
    out.bytes.push(b'\n');
    Ok(out.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturation_drops_whole_batches_and_preserves_sequence() {
        // Keep the receiver idle: saturation is deterministic, independent of disk speed.
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut recorder = Recorder {
            shard_id: Uuid::nil(),
            config: RecorderConfig {
                chunk_events: 1,
                ..Default::default()
            },
            pending: vec![],
            pending_bytes: 0,
            sequence: 0,
            loss: Loss::default(),
            sender: Some(sender),
            worker: None,
        };
        assert!(recorder.emit(Event::new(0, Payload::Heartbeat)).unwrap());
        assert!(!recorder.emit(Event::new(1, Payload::Heartbeat)).unwrap());
        assert_eq!(recorder.loss().queue_full, 1);
        receiver.recv().unwrap();
        assert!(recorder.emit(Event::new(2, Payload::Heartbeat)).unwrap());
        let Record::Chunk(chunk) = serde_json::from_slice(&receiver.recv().unwrap()).unwrap()
        else {
            panic!()
        };
        assert_eq!(chunk.sequence, 1);
        assert_eq!(chunk.loss.queue_full, 1);
        assert_eq!(chunk.events[0].timestamp_ns, 2);
    }

    #[test]
    fn serialization_is_size_bounded() {
        assert!(encode(&"x".repeat(MAX_EVENT_BYTES), MAX_EVENT_BYTES).is_err());
    }
}
