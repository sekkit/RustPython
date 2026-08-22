use crate::PyObject;
use crate::pymem::{PyMem_Calloc, PyMem_Free, PyMem_Malloc, PyMem_Realloc};
use crate::pystate::with_vm;
use core::ffi::{c_int, c_void};
use std::collections::HashSet;
use std::sync::Mutex;
use rustpython_vm::gc_state;

/// Global registry of raw buffer objects allocated by _PyObject_New for C extensions.
/// These are NOT valid RustPython objects — they're raw C structs allocated with libc::malloc.
/// Keys are stored as usize (pointers are Send+Sync when boxed as integers).
static FOREIGN_OBJECTS: once_cell::sync::Lazy<Mutex<HashSet<usize>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashSet::new()));

pub(crate) fn register_foreign_object(ptr: *const u8) {
    FOREIGN_OBJECTS.lock().unwrap().insert(ptr as usize);
}

pub(crate) fn is_foreign_object(ptr: *const u8) -> bool {
    FOREIGN_OBJECTS.lock().unwrap().contains(&(ptr as usize))
}

pub(crate) fn unregister_foreign_object(ptr: *const u8) {
    FOREIGN_OBJECTS.lock().unwrap().remove(&(ptr as usize));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_Track(op: *mut PyObject) {
    // Foreign raw-buffer objects (from _PyObject_New) have no GcPrefix;
    // tracking them would read/write unrelated heap memory.
    if crate::objimpl::is_foreign_object(op as *const u8) {
        return;
    }
    with_vm(|_vm| {
        let obj = unsafe { &*op };
        if !obj.is_gc_tracked() {
            unsafe { gc_state::gc_state().track_object(obj.into()) };
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_UnTrack(op: *mut PyObject) {
    if crate::objimpl::is_foreign_object(op as *const u8) {
        return;
    }
    with_vm(|_vm| {
        let obj = unsafe { &*op };
        if obj.is_gc_tracked() {
            unsafe { gc_state::gc_state().untrack_object(obj.into()) };
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_IsTracked(op: *mut PyObject) -> c_int {
    if crate::objimpl::is_foreign_object(op as *const u8) {
        return 0;
    }
    with_vm(|_vm| unsafe { (&*op).is_gc_tracked() }) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_IsFinalized(op: *mut PyObject) -> c_int {
    if crate::objimpl::is_foreign_object(op as *const u8) {
        return 0;
    }
    with_vm(|_vm| unsafe { (&*op).gc_finalized() }) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn PyGC_Collect() -> isize {
    let result = gc_state::gc_state().collect(2);
    (result.collected + result.uncollectable) as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn PyGC_Enable() -> c_int {
    let gc = gc_state::gc_state();
    let was_enabled = gc.is_enabled();
    gc.enable();
    was_enabled.into()
}

#[unsafe(no_mangle)]
pub extern "C" fn PyGC_Disable() -> c_int {
    let gc = gc_state::gc_state();
    let was_enabled = gc.is_enabled();
    gc.disable();
    was_enabled.into()
}

#[unsafe(no_mangle)]
pub extern "C" fn PyGC_IsEnabled() -> c_int {
    gc_state::gc_state().is_enabled().into()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Malloc(size: usize) -> *mut c_void {
    unsafe { PyMem_Malloc(size) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Calloc(nelem: usize, elsize: usize) -> *mut c_void {
    unsafe { PyMem_Calloc(nelem, elsize) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    unsafe { PyMem_Realloc(ptr, new_size) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Free(ptr: *mut c_void) {
    if is_foreign_object(ptr as *const u8) {
        unregister_foreign_object(ptr as *const u8);
        unsafe { libc::free(ptr) };
    } else {
        unsafe { PyMem_Free(ptr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_object_registry_roundtrip() {
        let buf = unsafe { libc::malloc(16) } as *const u8;
        assert!(!is_foreign_object(buf));
        register_foreign_object(buf);
        assert!(is_foreign_object(buf));
        // Re-registering the same pointer must not duplicate or corrupt state.
        register_foreign_object(buf);
        assert!(is_foreign_object(buf));
        unregister_foreign_object(buf);
        assert!(!is_foreign_object(buf));
        unregister_foreign_object(buf); // removing again is a no-op
        unsafe { libc::free(buf as *mut c_void) };
    }

    #[test]
    fn distinct_pointers_are_independent() {
        let a = unsafe { libc::malloc(8) } as *const u8;
        let b = unsafe { libc::malloc(8) } as *const u8;
        register_foreign_object(a);
        assert!(is_foreign_object(a));
        assert!(!is_foreign_object(b));
        unregister_foreign_object(a);
        assert!(!is_foreign_object(b));
        unsafe { libc::free(a as *mut c_void) };
        unsafe { libc::free(b as *mut c_void) };
    }
}
