//! Pointer-free CUDA/CUPTI observations; normalized nanoseconds use host CLOCK_MONOTONIC.
pub const MAGIC: u32 = 0x52504355;
pub const VERSION: u16 = 1;
pub const RUNTIME: u16 = 1;
pub const CAPABILITY: u16 = 2;
pub const DEVICE: u16 = 3;
pub const CONTEXT: u16 = 4;
pub const STREAM: u16 = 5;
pub const API_BEGIN: u16 = 6;
pub const API_END: u16 = 7;
pub const KERNEL: u16 = 8;
pub const MEMCPY: u16 = 9;
pub const MEMSET: u16 = 10;
pub const SYNC: u16 = 11;
pub const FINALIZE: u16 = 12;
pub const ERROR: u16 = 13;
pub const DROPPED: u16 = 14;
pub const EXTERNAL_CORRELATION: u16 = 15;

pub const TIMING_UNAVAILABLE: u32 = 1;
pub const NAME_TRUNCATED: u32 = 2;
pub const RESULT_KNOWN: u32 = 4;
pub const DEVICE_VISIBLE: u32 = 8;
pub const OPTIONAL: u32 = 16;
pub const SOURCE_ERROR: u32 = 32;
pub const FORCED_FLUSH: u32 = 64;
pub const ASYNC: u32 = 128;
pub const UNKNOWN_THREAD: u32 = 256;
pub const MONOTONIC_CLOCK: u32 = 1024;
pub const SOURCE_INCOMPLETE: u32 = 2048;
pub const UNKNOWN: u32 = u32::MAX;
pub const DOMAIN_RUNTIME: u32 = 1;
pub const DOMAIN_DRIVER: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CudaEvent {
    pub magic: u32,
    pub version: u16,
    pub kind: u16,
    pub pid: u32,
    pub tid: u32,
    pub instance: u64,
    pub activity_id: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub correlation: u64,
    pub runtime_correlation: u64,
    pub parent_correlation: u64,
    pub bytes: u64,
    pub graph_id: u64,
    pub graph_node_id: u64,
    pub value: i64,
    pub aux0: u64,
    pub aux1: u64,
    pub aux2: u64,
    pub device: u32,
    pub context: u32,
    pub stream: u32,
    pub src_device: u32,
    pub dst_device: u32,
    pub flags: u32,
    pub result: i32,
    pub api_domain: u32,
    pub operation: u32,
    pub grid: [u32; 3],
    pub block: [u32; 3],
    pub cluster: [u32; 3],
    pub registers: u32,
    pub reserved: u32,
    pub static_shared: u64,
    pub dynamic_shared: u64,
    pub name: [u8; 192],
    pub detail: [u8; 192],
}
pub const EVENT_SIZE: usize = 608;
const _: () = assert!(std::mem::size_of::<CudaEvent>() == EVENT_SIZE);
