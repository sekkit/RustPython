use crate::PyObject;
use crate::object::PyTypeObject;
use crate::object::define_py_check;
use crate::pystate::with_vm;
use crate::util::CStrExt;
use core::ffi::{c_char, c_int};
use rustpython_vm::function::{HeapMethodDef, PyMethodFlags};
use rustpython_vm::{PyRef, PyResult, VirtualMachine};

define_py_check!(fn PyCFunction_Check, types.builtin_function_or_method_type);
define_py_check!(exact fn PyCFunction_CheckExact, types.builtin_function_or_method_type);

#[repr(C)]
pub struct PyMethodDef {
    pub ml_name: *const c_char,
    pub ml_meth: PyMethodPointer,
    pub ml_flags: c_int,
    pub ml_doc: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
#[allow(non_snake_case)]
pub union PyMethodPointer {
    pub PyCFunction: unsafe extern "C" fn(slf: *mut PyObject, args: *mut PyObject) -> *mut PyObject,
    pub PyCFunctionWithKeywords: unsafe extern "C" fn(
        slf: *mut PyObject,
        args: *mut PyObject,
        kwargs: *mut PyObject,
    ) -> *mut PyObject,
    pub PyCFunctionFast: unsafe extern "C" fn(
        slf: *mut PyObject,
        args: *const *mut PyObject,
        nargs: isize,
    ) -> *mut PyObject,
    pub PyCFunctionFastWithKeywords: unsafe extern "C" fn(
        slf: *mut PyObject,
        args: *const *mut PyObject,
        nargs: isize,
        kwnames: *mut PyObject,
    ) -> *mut PyObject,
}

pub(crate) fn build_method_def(
    vm: &VirtualMachine,
    ml: &PyMethodDef,
    has_self: bool,
) -> PyResult<PyRef<HeapMethodDef>> {
    let name = unsafe { ml.ml_name.try_as_str(vm) }?;
    let doc = unsafe { ml.ml_doc.try_as_str_opt(vm) }?;
    let flags = PyMethodFlags::from_bits(ml.ml_flags as u32)
        .ok_or_else(|| vm.new_system_error("PyMethodDef contains unknown flags"))?;

    // The callable construction lives in the vm crate (shared with
    // _imp.create_dynamic, which builds module methods for C extensions).
    // ml_meth is a union; reading any variant yields the same pointer bits.
    let method = unsafe { ml.ml_meth.PyCFunction } as usize;
    rustpython_vm::import::build_c_method_def(vm, name, method, flags, has_self, doc)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCMethod_New(
    ml: *mut PyMethodDef,
    slf: *mut PyObject,
    _module: *mut PyObject,
    _cls: *mut PyTypeObject,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult {
        assert!(
            _cls.is_null(),
            "PyCMethod_New does not support METH_METHOD on abi3"
        );
        let ml = unsafe { &*ml };
        let zelf = unsafe { slf.as_ref().map(|obj| obj.to_owned()) };
        Ok(build_method_def(vm, ml, zelf.is_some())?
            .build_function(vm, zelf)
            .into())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_New(
    ml: *mut PyMethodDef,
    slf: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyCMethod_New(ml, slf, core::ptr::null_mut(), core::ptr::null_mut()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_NewEx(
    ml: *mut PyMethodDef,
    slf: *mut PyObject,
    module: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyCMethod_New(ml, slf, module, core::ptr::null_mut()) }
}

#[cfg(test)]
mod tests {
    use pyo3::exceptions::PyException;
    use pyo3::ffi::{PyLong_FromLong, PyObject};
    use pyo3::prelude::*;
    use pyo3::types::{PyCFunction, PyInt, PyString};

    #[test]
    fn closure_function() {
        Python::attach(|py| {
            let f = PyCFunction::new_closure(py, None, None, |_args, _kwargs| "Hello from Rust!")
                .unwrap();

            assert_eq!(
                f.call0().unwrap().cast::<PyString>().unwrap(),
                "Hello from Rust!"
            );
        })
    }

    #[test]
    fn function_no_args() {
        Python::attach(|py| {
            unsafe extern "C" fn c_fn(_self: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
                assert!(_self.is_null());
                assert!(_args.is_null());
                unsafe { PyLong_FromLong(4200) }
            }

            let py_fn = PyCFunction::new(py, c_fn, c"py_fn", c"", None).unwrap();

            let result = py_fn
                .call0()
                .unwrap()
                .cast::<PyInt>()
                .unwrap()
                .extract::<u32>()
                .unwrap();
            assert_eq!(result, 4200);

            assert!(py_fn.call((1,), None).is_err());
            assert!(py_fn.call((1, 2), None).is_err());
        })
    }

    #[test]
    fn closure_function_error() {
        Python::attach(|py| {
            let f = PyCFunction::new_closure(py, None, None, |_args, _kwargs| {
                Err::<(), _>(PyException::new_err("Something went wrong"))
            })
            .unwrap();

            let err = f.call0().unwrap_err();
            assert_eq!(
                err.value(py).repr().unwrap(),
                "Exception('Something went wrong')"
            );
        })
    }

    #[test]
    fn wrap_static_no_args_function() {
        #[pyfunction()]
        fn f() {}

        Python::attach(|py| {
            let module = PyModule::new(py, "test_wrap_pyfunction_forms").unwrap();

            let func = wrap_pyfunction!(f, &module).unwrap();
            func.call0().unwrap();
        });
    }
}
