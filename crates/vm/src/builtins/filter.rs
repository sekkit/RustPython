use super::{PyType, PyTypeRef};
use crate::{
    Context, Py, PyObjectRef, PyPayload, PyResult, VirtualMachine,
    class::PyClassImpl,
    object::{Traverse, TraverseFn},
    protocol::{PyIter, PyIterReturn},
    raise_if_stop,
    types::{Constructor, IterNext, Iterable, SelfIter},
};

#[pyclass(module = false, name = "filter", traverse = "manual", clear)]
#[derive(Debug)]
pub struct PyFilter {
    predicate: Option<PyObjectRef>,
    iterator: Option<PyIter>,
}

// SAFETY: traverse reports both owned references and clear moves both edges
// out of the payload before the object is deallocated.
unsafe impl Traverse for PyFilter {
    fn traverse(&self, tracer_fn: &mut TraverseFn<'_>) {
        self.predicate.traverse(tracer_fn);
        self.iterator.traverse(tracer_fn);
    }

    fn clear(&mut self, out: &mut Vec<PyObjectRef>) {
        if let Some(predicate) = self.predicate.take() {
            out.push(predicate);
        }
        if let Some(iterator) = self.iterator.take() {
            out.push(iterator.into());
        }
    }
}

impl PyPayload for PyFilter {
    #[inline]
    fn class(ctx: &Context) -> &'static Py<PyType> {
        ctx.types.filter_type
    }
}

impl Constructor for PyFilter {
    type Args = (PyObjectRef, PyIter);

    fn py_new(
        _cls: &Py<PyType>,
        (function, iterator): Self::Args,
        _vm: &VirtualMachine,
    ) -> PyResult<Self> {
        Ok(Self {
            predicate: Some(function),
            iterator: Some(iterator),
        })
    }
}

#[pyclass(with(IterNext, Iterable, Constructor), flags(BASETYPE))]
impl PyFilter {
    #[pymethod]
    fn __reduce__(&self, vm: &VirtualMachine) -> PyResult<(PyTypeRef, (PyObjectRef, PyIter))> {
        let Some(predicate) = self.predicate.as_ref() else {
            return Err(vm.new_runtime_error("cannot reduce a cleared filter"));
        };
        let Some(iterator) = self.iterator.as_ref() else {
            return Err(vm.new_runtime_error("cannot reduce a cleared filter"));
        };
        Ok((
            vm.ctx.types.filter_type.to_owned(),
            (predicate.clone(), iterator.clone()),
        ))
    }
}

impl SelfIter for PyFilter {}

impl IterNext for PyFilter {
    fn next(zelf: &Py<Self>, vm: &VirtualMachine) -> PyResult<PyIterReturn> {
        let (Some(predicate), Some(iterator)) =
            (zelf.predicate.as_ref(), zelf.iterator.as_ref())
        else {
            return Ok(PyIterReturn::StopIteration(None));
        };
        loop {
            let next_obj = raise_if_stop!(iterator.next(vm)?);
            let predicate_value = if vm.is_none(predicate) {
                next_obj.clone()
            } else {
                // the predicate itself can raise StopIteration which does stop the filter iteration
                raise_if_stop!(PyIterReturn::from_pyresult(
                    predicate.call((next_obj.clone(),), vm),
                    vm
                )?)
            };

            if predicate_value.try_to_bool(vm)? {
                return Ok(PyIterReturn::Return(next_obj));
            }
        }
    }
}

pub(crate) fn init(context: &'static Context) {
    PyFilter::extend_class(context, context.types.filter_type);
}
