//! Bounded streaming storage, validation, recovery, and shard merging.

mod merge;
mod reader;
mod writer;

pub use merge::merge;
pub use reader::{Reader, ShardSummary, Summary, summarize};
pub use writer::{Recorder, RecorderConfig};

/// Maximum encoded frame, including its newline. Readers enforce the same bound
/// before JSON parsing; corrupt files cannot request unbounded frame allocation.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_SHARDS: usize = 4096;
pub const MAX_TRACKS: usize = 65_536;
