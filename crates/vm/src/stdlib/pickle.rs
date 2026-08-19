use crate::{
    AsObject, Py, PyObjectRef, PyPayload, PyResult, TryFromBorrowedObject, VirtualMachine,
    builtins::{PyBytesRef, PyMemoryView, PyType},
    class::PyClassImpl,
    common::lock::PyMutex,
    protocol::PyBuffer,
    types::{AsBuffer, Constructor, Representable},
};

/// Extend the `PickleBuffer` static type with its methods and slots.
///
/// Called from `TypeZoo::extend` after all builtin types are registered;
/// without this the type would lack `getattro` and its Python methods.
///
/// `PickleBuffer` is exposed as a builtin (CPython 3.14 does the same) rather
/// than through a partial `_pickle` module: RustPython has no `_pickle` C
/// accelerator, so registering one would break `test_pickle.py`'s
/// `has_c_implementation` detection.
pub(crate) fn init(ctx: &'static crate::vm::Context) {
    PyPickleBuffer::extend_class(ctx, ctx.types.pickle_buffer_type);
}

#[pyclass(module = "builtins", name = "PickleBuffer", traverse)]
#[derive(Debug, PyPayload)]
pub(crate) struct PyPickleBuffer {
    buffer: PyMutex<Option<PyBuffer>>,
}

#[derive(FromArgs)]
pub(crate) struct PickleBufferArgs {
    object: PyObjectRef,
}

impl Constructor for PyPickleBuffer {
    type Args = PickleBufferArgs;

    fn py_new(_cls: &Py<PyType>, args: Self::Args, vm: &VirtualMachine) -> PyResult<Self> {
        Ok(Self {
            buffer: PyMutex::new(Some(PyBuffer::try_from_borrowed_object(vm, &args.object)?)),
        })
    }
}

#[pyclass(
    with(Constructor, AsBuffer, Representable),
    flags(HAS_WEAKREF, MANAGED_WEAKREF)
)]
impl PyPickleBuffer {
    #[pymethod]
    fn release(&self) {
        self.buffer.lock().take();
    }

    #[pymethod]
    fn raw(&self, vm: &VirtualMachine) -> PyResult {
        let buffer = self.buffer.lock();
        let buffer = buffer.as_ref().ok_or_else(|| {
            vm.new_value_error("operation forbidden on released PickleBuffer object")
        })?;
        if !buffer.desc.is_contiguous() {
            return Err(
                vm.new_buffer_error("cannot return the raw buffer of a non-contiguous buffer")
            );
        }
        PyMemoryView::from_buffer(buffer.clone(), vm).map(|view| view.into_pyobject(vm))
    }

    #[pymethod]
    fn __bytes__(&self, vm: &VirtualMachine) -> PyResult<PyBytesRef> {
        let buffer = self.buffer.lock();
        let buffer = buffer.as_ref().ok_or_else(|| {
            vm.new_value_error("operation forbidden on released PickleBuffer object")
        })?;
        let mut bytes = Vec::with_capacity(buffer.desc.len);
        buffer.append_to(&mut bytes);
        Ok(vm.ctx.new_bytes(bytes))
    }
}

impl AsBuffer for PyPickleBuffer {
    fn as_buffer(zelf: &Py<Self>, vm: &VirtualMachine) -> PyResult<PyBuffer> {
        zelf.buffer
            .lock()
            .as_ref()
            .map(PyBuffer::clone)
            .ok_or_else(|| {
                vm.new_value_error("operation forbidden on released PickleBuffer object")
            })
    }
}

impl Representable for PyPickleBuffer {
    fn repr_str(zelf: &Py<Self>, _vm: &VirtualMachine) -> PyResult<String> {
        Ok(format!(
            "<pickle.PickleBuffer object at {:#x}>",
            zelf.as_object().get_id()
        ))
    }
}
