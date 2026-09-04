use crate::{PyObject, pystate::with_vm};
use core::ptr::NonNull;
use rustpython_vm::PyObjectRef;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_DecRef(op: *mut PyObject) {
    // By dropping PyObjectRef, we will decrement the reference count.
    unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(op))) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_IncRef(op: *mut PyObject) {
    // Don't drop the owned value, as we just want to increment the refcount.
    core::mem::forget(unsafe { (*op).to_owned() });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_NewRef(op: *mut PyObject) -> *mut PyObject {
    with_vm(|_vm| unsafe { (*op).to_owned() })
}

/// _Py_Dealloc: called by CPython's inline Py_DECREF when the C-visible
/// refcount reaches zero. Our objects carry the immortal flag bit from the
/// C side (see crates/common/src/refcount.rs), so inline decrefs are no-ops
/// and this is normally unreachable for native objects; for foreign (raw
/// buffer) objects we must free the libc-allocated memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_Dealloc(op: *mut PyObject) {
    if crate::objimpl::is_foreign_object(op as *const u8) {
        // Foreign object: free the raw buffer allocated by _PyObject_New.
        // A foreign CPython-layout *list* buffer also owns a separately
        // malloc'd ob_item array plus one strong reference per slot; release
        // those first so no element outlives its list.
        if unsafe { crate::listobject::is_foreign_list_buffer(op) } {
            unsafe { free_foreign_list(op) };
            return;
        }
        crate::objimpl::unregister_foreign_object(op as *const u8);
        unsafe { libc::free(op as *mut core::ffi::c_void) };
    } else {
        unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(op))) };
    }
}

/// Release a foreign CPython-layout list buffer: decref all slots, then free
/// the ob_item array and the header. Caller holds a `&mut` on the object, so
/// this must not be re-entrant.
///
/// # Safety
/// `op` must be a valid foreign list buffer (see
/// `listobject::is_foreign_list_buffer`); must only be called from
/// `_Py_Dealloc` on an object whose refcount just reached zero.
unsafe fn free_foreign_list(op: *mut PyObject) {
    // Deregister first: no future lookup may observe a pointer about to be
    // freed, and reentrant allocations won't see a live foreign entry.
    crate::objimpl::unregister_foreign_object(op as *const u8);
    let size = unsafe { crate::listobject::header_obj_size(op) };
    let items = unsafe { crate::listobject::header_obj_item(op) };
    for i in 0..size {
        let item = unsafe { *items.offset(i as isize) };
        if !item.is_null() {
            unsafe {
                core::mem::drop(PyObjectRef::from_raw(NonNull::new_unchecked(item)));
            }
        }
    }
    unsafe {
        libc::free(items as *mut core::ffi::c_void);
        libc::free(op as *mut core::ffi::c_void);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_REFCNT(op: *mut PyObject) -> isize {
    with_vm(|_vm| unsafe { &*op }.strong_count())
}

#[cfg(test)]
mod tests {
    use pyo3::ffi;
    use pyo3::prelude::*;
    use pyo3::types::PyList;

    #[test]
    fn refcount() {
        Python::attach(|py| unsafe {
            // A freshly created, non-empty list is uniquely owned here: its
            // reference count is private to this test (so parallel tests cannot
            // perturb it) and it is mortal (not interned), so incref then decref
            // must move the count by exactly one and back.
            let obj = PyList::new(py, [1, 2, 3]).unwrap();
            let ref_count = ffi::Py_REFCNT(obj.as_ptr());
            let obj_clone = obj.clone();
            assert_eq!(ffi::Py_REFCNT(obj.as_ptr()), ref_count + 1);
            drop(obj_clone);
            assert_eq!(ffi::Py_REFCNT(obj.as_ptr()), ref_count);
        });
    }
}
