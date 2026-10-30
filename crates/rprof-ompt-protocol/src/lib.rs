//! Pointer-free OMPT host observations. Code addresses are opaque attribution
//! values, never dereferenced by the collector or browser.
pub const MAGIC: u32 = 0x52504f4d;
pub const VERSION: u16 = 1;
pub const RUNTIME: u16 = 1;
pub const CAPABILITY: u16 = 2;
pub const THREAD_BEGIN: u16 = 3;
pub const THREAD_END: u16 = 4;
pub const PARALLEL_BEGIN: u16 = 5;
pub const PARALLEL_END: u16 = 6;
pub const IMPLICIT_BEGIN: u16 = 7;
pub const IMPLICIT_END: u16 = 8;
pub const WORK_BEGIN: u16 = 9;
pub const WORK_END: u16 = 10;
pub const SYNC_BEGIN: u16 = 11;
pub const SYNC_END: u16 = 12;
pub const WAIT_BEGIN: u16 = 13;
pub const WAIT_END: u16 = 14;
pub const TASK_CREATE: u16 = 15;
pub const TASK_SCHEDULE: u16 = 16;
pub const DEPENDENCE: u16 = 17;
pub const FINALIZE: u16 = 18;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OmpEvent {
    pub magic: u32,
    pub version: u16,
    pub kind: u16,
    pub pid: u32,
    pub tid: u32,
    pub instance: u64,
    pub timestamp_ns: u64,
    pub sequence: u64,
    pub thread_id: u64,
    pub parallel_id: u64,
    pub task_id: u64,
    pub parent_task_id: u64,
    pub next_task_id: u64,
    pub codeptr: u64,
    pub count: u64,
    pub flags: u64,
    pub value: i64,
    pub aux: i64,
    pub name: [u8; 64],
    pub detail: [u8; 192],
}
pub const EVENT_SIZE: usize = 376;
const _: () = assert!(std::mem::size_of::<OmpEvent>() == EVENT_SIZE);
