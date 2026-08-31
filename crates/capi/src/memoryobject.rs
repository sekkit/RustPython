use crate::buffer::Py_buffer;
use crate::object::define_py_check;
use crate::{PyObject, pystate::with_vm};
use core::ffi::{c_char, c_int};
use rustpython_vm::PyPayload;
use rustpython_vm::builtins::PyMemoryView;
use rustpython_vm::protocol::{CPyBuffer, PyBuffer, pybuffer_from_c_view};

define_py_check!(fn PyMemoryView_Check, types.memoryview_type);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromObject(obj: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { &*obj };
        Ok(PyMemoryView::from_object(obj, vm)?.into_ref(&vm.ctx))
    })
}

/// PyMemoryView_FromBuffer: create a memoryview from a `Py_buffer`. Takes
/// ownership of `view->obj`; the caller must not release `view` afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromBuffer(view: *mut Py_buffer) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        if view.is_null() {
            return Err(vm.new_system_error("PyMemoryView_FromBuffer called with NULL view"));
        }
        let cview = unsafe { core::ptr::read(view as *const CPyBuffer) };
        let buf = pybuffer_from_c_view(cview, vm)?;
        let mv = PyMemoryView::from_buffer(buf, vm)?;
        let mv_ref: rustpython_vm::PyObjectRef = mv.into_ref(&vm.ctx).into();
        Ok(mv_ref.into_raw().as_ptr())
    })
}

// Anchor so the linker keeps PyMemoryView_FromBuffer in the export table.
#[used]
static PY_MEMORYVIEW_FROM_BUFFER_ANCHOR: unsafe extern "C" fn(*mut Py_buffer) -> *mut PyObject =
    PyMemoryView_FromBuffer;

/// Rust implementation of the C shim's PyMemoryView_FromMemory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_memoryview_from_memory(
    data: *const c_char,
    size: isize,
    format: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<rustpython_vm::PyObjectRef> {
        if data.is_null() {
            return Err(vm.new_system_error("PyMemoryView_FromMemory called with NULL data"));
        }
        let len = size.max(0) as usize;
        // Create a bytes object as the backing store for the memoryview.
        // This ensures the exporter is a valid Python object.
        let data_slice = unsafe { core::slice::from_raw_parts(data as *const u8, len) };
        let bytes = vm.ctx.new_bytes(data_slice.to_vec());
        // Create a memoryview from the bytes object.
        let bytes_obj: rustpython_vm::PyObjectRef = bytes.into();
        let mv = PyMemoryView::from_object(&bytes_obj, vm)?;
        Ok(mv.into_ref(&vm.ctx).into())
    })
}

/// PyMemoryView_GetContiguous: return a contiguous memoryview for the given
/// object. Matches CPython's memoryview_get_contiguous.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_GetContiguous(
    obj: *mut PyObject,
    _buffertype: c_int,
    _order: c_char,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        let obj = unsafe { &*obj };
        // Create a memoryview from the object.
        let mv = PyMemoryView::from_object(obj, vm)?;
        // Get a contiguous buffer (copies if needed via to_contiguous).
        let buf = mv.to_contiguous(vm);
        // Wrap the contiguous buffer in a new memoryview.
        let result = PyMemoryView::from_buffer(buf, vm)?;
        let result_ref = result.into_ref(&vm.ctx);
        let result_obj: rustpython_vm::PyObjectRef = result_ref.into();
        Ok(result_obj)
    })
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;
    use pyo3::types::{PyBytes, PyMemoryView};

    #[test]
    fn memoryview_from_bytes() {
        Python::attach(|py| {
            let bytes = PyBytes::new(py, b"hello");
            let view = PyMemoryView::from(&bytes).unwrap();

            assert!(view.is_instance_of::<PyMemoryView>());

            let copied = view
                .call_method1("tobytes", ())
                .unwrap()
                .cast_into::<PyBytes>()
                .unwrap();
            assert_eq!(copied.as_bytes(), b"hello");
        })
    }
}
