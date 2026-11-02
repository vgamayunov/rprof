//! Single-request, owned-buffer ABI for the browser. Allocation sizes are capped.
use std::cell::RefCell;
thread_local! { static OUTPUT:RefCell<Vec<u8>>=const{RefCell::new(Vec::new())}; }

#[unsafe(no_mangle)]
pub extern "C" fn projection_alloc(len: u32) -> *mut u8 {
    if len as usize > crate::MAX_REQUEST_BYTES {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0u8; len as usize].into_boxed_slice()).cast::<u8>()
}
/// # Safety
/// Pointer and length must come from projection_alloc, and be submitted once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn projection_run(ptr: *mut u8, len: u32) -> u32 {
    if ptr.is_null() || len as usize > crate::MAX_REQUEST_BYTES {
        return 0;
    }
    let input = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len as usize)) };
    let result = match crate::project_json(&input) {
        Ok(value) => value,
        Err(e) => serde_json::to_vec(&serde_json::json!({"error":format!("{e:#}")})).unwrap(),
    };
    OUTPUT.with(|out| {
        let mut out = out.borrow_mut();
        *out = result;
        out.len() as u32
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn projection_output() -> *const u8 {
    OUTPUT.with(|out| out.borrow().as_ptr())
}
