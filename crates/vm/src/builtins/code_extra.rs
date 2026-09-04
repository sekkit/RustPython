//! Global registry for code-object "extra index" free callbacks.
//!
//! CPython's unstable API `PyUnstable_Eval_RequestCodeExtraIndex(free)` hands
//! out unique indices that C extensions use to stash per-code-object data via
//! `PyUnstable_Code_SetExtra`/`PyUnstable_Code_GetExtra`. Each index has an
//! optional `free` callback that is invoked with the stored pointer when the
//! code object is deallocated.
//!
//! The free callback is a plain C function pointer (never a `PyObjectRef`), so
//! a plain `std::sync::Mutex` suffices here rather than `PyMutex`. The registry
//! must live in the `vm` crate because the `capi` crate depends on `vm` (not
//! the other way around).

use core::ffi::c_void;
use std::sync::Mutex;

/// A `free` callback registered via `PyUnstable_Eval_RequestCodeExtraIndex`.
pub type CodeExtraFreeFunc = unsafe extern "C" fn(*mut c_void);

static CODE_EXTRA_FREEFUNCS: Mutex<Vec<Option<CodeExtraFreeFunc>>> = Mutex::new(Vec::new());

/// Acquire the registry table, recovering from a previously poisoned mutex.
fn table() -> std::sync::MutexGuard<'static, Vec<Option<CodeExtraFreeFunc>>> {
    CODE_EXTRA_FREEFUNCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Register `free` (optional) and return its unique index.
pub fn request_code_extra_index(free: Option<CodeExtraFreeFunc>) -> isize {
    let mut table = table();
    let index = table.len() as isize;
    table.push(free);
    index
}

/// Number of registered extra indices.
pub(crate) fn code_extra_len() -> isize {
    table().len() as isize
}

/// The free callback registered for `index`, or `None` if `index` is out of
/// range or was registered with no callback.
pub(crate) fn code_extra_freefunc(index: isize) -> Option<CodeExtraFreeFunc> {
    if index < 0 {
        return None;
    }
    table().get(index as usize).copied().flatten()
}

/// Call the free callbacks for every populated slot of a deallocating code
/// object. Each slot value is an opaque pointer stored as `usize`.
pub(crate) fn free_slots(slots: Vec<Option<usize>>) {
    for (index, slot) in slots.into_iter().enumerate() {
        let Some(ptr) = slot else { continue };
        let Some(free) = code_extra_freefunc(index as isize) else {
            continue;
        };
        // SAFETY: `ptr` was stored through PyUnstable_Code_SetExtra and is
        // reclaimed exactly once by the callback registered for its index.
        unsafe { free(ptr as *mut c_void) };
    }
}
