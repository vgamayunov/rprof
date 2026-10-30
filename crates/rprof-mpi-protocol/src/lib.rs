//! Same-host fixed MPI observations. The C adapter is compiled against the target
//! MPI headers; opaque MPI handles never cross this boundary.

pub const MAGIC: u32 = 0x52504d50;
pub const VERSION: u16 = 1;
pub const CALL: u16 = 1;
pub const REQUEST_BEGIN: u16 = 2;
pub const REQUEST_END: u16 = 3;
pub const COMM: u16 = 4;
pub const CLOCK: u16 = 5;
pub const METADATA: u16 = 6;
pub const BYTES_KNOWN: u32 = 1;
pub const CANCELLED: u32 = 2;
pub const DETACHED: u32 = 4;
pub const TRACKING_LIMIT: u32 = 8;
pub const DETAIL_TRUNCATED: u32 = 16;
pub const ABORT_ENTRY: u32 = 32;
pub const REQUEST_AMBIGUOUS: u32 = 64;
pub const INTERCOMM: u32 = 128;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpiEvent {
    pub magic: u32,
    pub version: u16,
    pub kind: u16,
    pub pid: u32,
    pub tid: u32,
    pub instance: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub call_id: u64,
    pub request_id: u64,
    pub comm_id: u64,
    pub bytes: u64,
    pub count: i64,
    pub peer: i32,
    pub tag: i32,
    pub rank: i32,
    pub world_size: i32,
    pub comm_rank: i32,
    pub comm_size: i32,
    pub result: i32,
    pub flags: u32,
    pub thread_level: i32,
    pub reserved: u32,
    pub aux0: i64,
    pub aux1: u64,
    pub aux2: u64,
    pub aux3: u64,
    pub name: [u8; 64],
    pub detail: [u8; 256],
}

pub const EVENT_SIZE: usize = 472;
const _: () = assert!(std::mem::size_of::<MpiEvent>() == EVENT_SIZE);
