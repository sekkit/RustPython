//! Minimal buffer protocol (CPython Objects/abstract.c, buffer.c).
//!
//! Only the flat 1-dimensional case is modeled: bytes and bytearray expose
//! their storage, everything else reports that it does not support the buffer
//! interface.

use crate::PyObject;
use crate::pystate::with_vm;
use crate::refcount::_Py_DecRef;
use core::ffi::{c_char, c_int, c_void};
use rustpython_vm::builtins::{PyByteArray, PyBytes};
use rustpython_vm::{AsObject, PyResult};

// Names intentionally mirror the C identifiers.
#[allow(non_upper_case_globals, dead_code, unreachable_pub)]
mod buffer_flags {
    use core::ffi::c_int;

    pub const PyBUF_SIMPLE: c_int = 0;
    pub const PyBUF_WRITABLE: c_int = 0x0001;
    pub const PyBUF_FORMAT: c_int = 0x0004;
    pub const PyBUF_ND: c_int = 0x0008;
    pub const PyBUF_STRIDES: c_int = 0x0010 | PyBUF_ND;
}
use buffer_flags::*;

/// Layout mirrors CPython's Include/cpython/object.h `Py_buffer`.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Py_buffer {
    pub buf: *mut c_void,
    /// Owned reference.
    pub obj: *mut PyObject,
    pub len: isize,
    pub itemsize: isize,
    pub readonly: c_int,
    pub ndim: c_int,
    pub format: *mut c_char,
    pub shape: *mut isize,
    pub strides: *mut isize,
    pub suboffsets: *mut isize,
    pub internal: *mut c_void,
}

/// PyObject_GetBuffer: fill `view` with the object's buffer. Supports bytes
/// and bytearray; other objects raise TypeError like CPython.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetBuffer(
    obj: *mut PyObject,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    with_vm(|vm| -> PyResult<c_int> {
        if view.is_null() {
            return Err(vm.new_system_error("PyObject_GetBuffer called with a NULL view"));
        }
        let obj = unsafe { &*obj };
        let (ptr, len, readonly) = if let Some(b) = obj.downcast_ref::<PyBytes>() {
            let data = b.as_bytes();
            (data.as_ptr() as *mut c_void, data.len() as isize, 1)
        } else if let Some(ba) = obj.downcast_ref::<PyByteArray>() {
            let data = ba.borrow_buf();
            (data.as_ptr() as *mut c_void, data.len() as isize, 0)
        } else {
            return Err(vm.new_type_error(format!(
                "a bytes-like object is required, not '{}'",
                obj.class().name()
            )));
        };
        if flags & PyBUF_WRITABLE != 0 && readonly != 0 {
            return Err(vm.new_buffer_error("Object is not writable."));
        }
        let view = unsafe { &mut *view };
        view.buf = ptr;
        // Owned reference: bump the refcount and keep it in the view.
        let owned = obj.to_owned();
        view.obj = owned.as_object().as_raw().cast_mut();
        core::mem::forget(owned);
        view.len = len;
        view.itemsize = 1;
        view.readonly = readonly;
        view.ndim = 1;
        view.format = if flags & PyBUF_FORMAT != 0 {
            c"B".as_ptr().cast_mut()
        } else {
            core::ptr::null_mut()
        };
        view.shape = core::ptr::null_mut();
        view.strides = core::ptr::null_mut();
        view.suboffsets = core::ptr::null_mut();
        view.internal = core::ptr::null_mut();
        Ok(0)
    })
}

/// PyBuffer_Release: drop the reference held in the view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_Release(view: *mut Py_buffer) {
    if view.is_null() {
        return;
    }
    let view = unsafe { &mut *view };
    if !view.obj.is_null() {
        unsafe { _Py_DecRef(view.obj) };
    }
    *view = Py_buffer {
        buf: core::ptr::null_mut(),
        obj: core::ptr::null_mut(),
        len: 0,
        itemsize: 0,
        readonly: 0,
        ndim: 0,
        format: core::ptr::null_mut(),
        shape: core::ptr::null_mut(),
        strides: core::ptr::null_mut(),
        suboffsets: core::ptr::null_mut(),
        internal: core::ptr::null_mut(),
    };
}

/// PyBuffer_GetPointer: compute the data pointer for the given indices.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_GetPointer(
    view: *const Py_buffer,
    indices: *const isize,
) -> *mut c_void {
    let view = unsafe { &*view };
    if view.ndim == 0 {
        return view.buf;
    }
    let indices = unsafe { core::slice::from_raw_parts(indices, view.ndim as usize) };
    let mut pointer = view.buf.cast::<u8>();
    if view.strides.is_null() {
        for &idx in indices {
            pointer = unsafe { pointer.add((view.itemsize * idx) as usize) };
        }
    } else {
        let strides = unsafe { core::slice::from_raw_parts(view.strides, view.ndim as usize) };
        for (&stride, &idx) in strides.iter().zip(indices) {
            pointer = unsafe { pointer.offset(stride * idx) };
        }
    }
    pointer as *mut c_void
}
