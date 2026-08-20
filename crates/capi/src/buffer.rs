//! Buffer protocol (CPython Objects/abstract.c, buffer.c).
//!
//! Dispatches through the VM's buffer protocol, which handles native
//! `as_buffer` slots (bytes, bytearray, etc.) and C extensions through
//! `CBufferSlots`.

use crate::PyObject;
use crate::pystate::with_vm;
use crate::refcount::{_Py_DecRef, _Py_IncRef};
use alloc::ffi::CString;
use core::ffi::{c_char, c_int, c_void};
use rustpython_vm::protocol::PyBuffer;
use rustpython_vm::{AsObject, PyResult, TryFromBorrowedObject};

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
pub(crate) use buffer_flags::*;

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

/// PyObject_GetBuffer: fill `view` with the object's buffer.
/// Dispatches through the VM's buffer protocol, so any buffer-supporting
/// object (bytes, bytearray, memoryview, C extensions with CBufferSlots)
/// is handled.
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
        let buf = PyBuffer::try_from_borrowed_object(vm, obj)?;
        if flags & PyBUF_WRITABLE != 0 && buf.desc.readonly {
            return Err(vm.new_buffer_error("Object is not writable."));
        }

        // Read the data pointer under a transient borrow (same pattern as
        // the previous bytes/bytearray fast path: the pointer stays valid
        // while the exporter is alive, which `view.obj` guarantees).
        let ptr = buf.obj_bytes().as_ptr() as *mut c_void;
        let view = unsafe { &mut *view };
        view.buf = ptr;
        view.len = buf.desc.len as isize;
        view.itemsize = buf.desc.itemsize.max(1) as isize;
        view.readonly = if buf.desc.readonly { 1 } else { 0 };
        view.ndim = buf.desc.ndim() as c_int;

        // Format string (CString, freed in PyBuffer_Release).
        let fmt = buf.desc.format.as_bytes();
        if fmt != b"B" && !fmt.is_empty() {
            // Format strings do not contain NUL bytes; build a C string.
            let cstr = CString::new(fmt).unwrap_or_else(|_| CString::new(b"B").unwrap());
            view.format = cstr.into_raw();
        } else {
            view.format = core::ptr::null_mut();
        }

        // Shape and strides arrays (allocated with Box, freed in PyBuffer_Release).
        let ndim = buf.desc.ndim();
        if ndim > 0 {
            let mut shape = vec![0isize; ndim].into_boxed_slice();
            let mut strides = vec![0isize; ndim].into_boxed_slice();
            for (i, (s, stride, _sub)) in buf.desc.dim_desc.iter().enumerate() {
                shape[i] = *s as isize;
                strides[i] = *stride;
            }
            view.shape = Box::leak(shape).as_mut_ptr();
            view.strides = Box::leak(strides).as_mut_ptr();
        } else {
            view.shape = core::ptr::null_mut();
            view.strides = core::ptr::null_mut();
        }
        view.suboffsets = core::ptr::null_mut();
        view.internal = core::ptr::null_mut();

        // Owned reference: the Rust `PyBuffer` drops after this function
        // (its own export counter is released), but the C view keeps the
        // exporter alive via `view.obj`.
        let owned = buf.obj.clone();
        view.obj = owned.as_object().as_raw().cast_mut();
        core::mem::forget(owned);
        Ok(0)
    })
}

/// PyBuffer_Release: drop the reference held in the view, plus the shape /
/// strides / format arrays that PyObject_GetBuffer allocated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_Release(view: *mut Py_buffer) {
    if view.is_null() {
        return;
    }
    let view = unsafe { &mut *view };
    if !view.obj.is_null() {
        unsafe { _Py_DecRef(view.obj) };
    }
    if !view.shape.is_null() {
        // Recover the Box allocations made in PyObject_GetBuffer.
        unsafe {
            drop(Box::from_raw(core::slice::from_raw_parts_mut(view.shape, view.ndim as usize)));
        }
        view.shape = core::ptr::null_mut();
    }
    if !view.strides.is_null() {
        unsafe {
            drop(Box::from_raw(core::slice::from_raw_parts_mut(view.strides, view.ndim as usize)));
        }
        view.strides = core::ptr::null_mut();
    }
    if !view.format.is_null() {
        unsafe {
            drop(CString::from_raw(view.format));
        }
        view.format = core::ptr::null_mut();
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

/// PyBuffer_IsContiguous: check if a Py_buffer is contiguous in the given
/// order ('C' = row-major, 'F' = column-major, 'A' = any).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_IsContiguous(
    view: *const Py_buffer,
    order: c_char,
) -> c_int {
    let view = unsafe { &*view };
    if view.ndim == 0 || view.len == 0 {
        return 1;
    }
    // If strides are null, the buffer is 1-D and contiguous.
    if view.strides.is_null() {
        return 1;
    }
    let strides = unsafe { core::slice::from_raw_parts(view.strides, view.ndim as usize) };
    let shape = if view.shape.is_null() {
        // No shape: 1-D, contiguous by definition.
        return 1;
    } else {
        unsafe { core::slice::from_raw_parts(view.shape, view.ndim as usize) }
    };
    match order as u8 as char {
        'C' => {
            let mut sd = view.itemsize;
            for i in (0..view.ndim as usize).rev() {
                if shape[i] > 1 && strides[i] != sd {
                    return 0;
                }
                sd *= shape[i];
            }
            1
        }
        'F' => {
            let mut sd = view.itemsize;
            for i in 0..view.ndim as usize {
                if shape[i] > 1 && strides[i] != sd {
                    return 0;
                }
                sd *= shape[i];
            }
            1
        }
        'A' => {
            // Check if it's C-contiguous or F-contiguous.
            // First check C-contiguous.
            let mut sd = view.itemsize;
            let mut c_contig = true;
            for i in (0..view.ndim as usize).rev() {
                if shape[i] > 1 && strides[i] != sd {
                    c_contig = false;
                    break;
                }
                sd *= shape[i];
            }
            if c_contig {
                return 1;
            }
            // Then check F-contiguous.
            sd = view.itemsize;
            for i in 0..view.ndim as usize {
                if shape[i] > 1 && strides[i] != sd {
                    return 0;
                }
                sd *= shape[i];
            }
            1
        }
        _ => 0,
    }
}

/// PyObject_CheckBuffer: return 1 if the object supports the buffer protocol,
/// 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CheckBuffer(obj: *mut PyObject) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        let obj = unsafe { &*obj };
        // Try to get a buffer; if it succeeds, the object supports the protocol.
        match PyBuffer::try_from_borrowed_object(vm, obj) {
            Ok(_) => Ok(1),
            Err(_) => Ok(0),
        }
    })
}

/// PyBuffer_ToContiguous: copy the buffer data to a contiguous destination.
/// Returns 0 on success, -1 on error (with an exception set).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_ToContiguous(
    dst: *mut c_void,
    src: *mut Py_buffer,
    len: isize,
    order: c_char,
) -> c_int {
    let src = unsafe { &*src };
    let ndim = src.ndim as usize;
    let itemsize = if src.itemsize > 0 { src.itemsize as usize } else { 1 };
    let copy_len = (len.min(src.len).max(0) as usize) / itemsize;

    if ndim <= 1 || src.strides.is_null() {
        // 1-D or no strides: simple copy.
        let bytes = copy_len * itemsize;
        unsafe {
            core::ptr::copy_nonoverlapping(src.buf.cast::<u8>(), dst.cast::<u8>(), bytes);
        }
        return 0;
    }

    // N-D with strides: walk the logical elements in the requested order.
    let shape = unsafe { core::slice::from_raw_parts(src.shape, ndim) };
    let strides = unsafe { core::slice::from_raw_parts(src.strides, ndim) };
    let f_order = order as u8 as char == 'F';

    let mut indices = vec![0isize; ndim];
    let src_base = src.buf.cast::<u8>();
    let dst_base = dst.cast::<u8>();
    let mut copied = 0usize;
    loop {
        // Compute the byte offset for the current index vector.
        let mut src_off = 0isize;
        for (i, &idx) in indices.iter().enumerate() {
            src_off += strides[i] * idx;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(src_base.offset(src_off), dst_base.add(copied * itemsize), itemsize);
        }
        copied += 1;
        if copied >= copy_len {
            break;
        }
        // Advance the index vector (dim 0 fastest for F, last dim fastest for C).
        let first = if f_order { 0 } else { ndim - 1 };
        let step = if f_order { 1 } else { -1 };
        let mut d = first;
        loop {
            indices[d] += step;
            if indices[d] >= 0 && indices[d] < shape[d] {
                break;
            }
            indices[d] = 0;
            if (f_order && d + 1 >= ndim) || (!f_order && d == 0) {
                return 0; // wrapped past the end
            }
            d = (d as isize + step) as usize;
        }
    }
    0
}

/// PyBuffer_FillInfo: fill a Py_buffer from a simple contiguous buffer.
/// This is a helper for C extensions that want to export a 1-D contiguous
/// buffer without implementing getbufferproc themselves.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_FillInfo(
    view: *mut Py_buffer,
    exporter: *mut PyObject,
    buf: *mut c_void,
    len: isize,
    readonly: c_int,
    infoflags: c_int,
) -> c_int {
    // If the exporter is non-NULL, incref it (the view will hold a reference).
    if !exporter.is_null() {
        unsafe { _Py_IncRef(exporter) };
    }
    let view = unsafe { &mut *view };
    view.buf = buf;
    view.obj = exporter;
    view.len = len;
    view.itemsize = 1;
    view.readonly = readonly;
    view.ndim = 1;
    view.format = if infoflags & PyBUF_FORMAT != 0 {
        // CPython returns "B" for bytes-like format.
        c"B".as_ptr() as *mut c_char
    } else {
        core::ptr::null_mut()
    };
    view.shape = core::ptr::null_mut();
    view.strides = core::ptr::null_mut();
    view.suboffsets = core::ptr::null_mut();
    view.internal = core::ptr::null_mut();
    0
}
