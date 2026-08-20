use crate::buffer::Py_buffer;
use crate::object::define_py_check;
use crate::{PyObject, pystate::with_vm};
use rustpython_vm::PyPayload;
use rustpython_vm::builtins::PyMemoryView;
use rustpython_vm::protocol::{CPyBuffer, pybuffer_from_c_view};

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
