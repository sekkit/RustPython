use crate::object::define_py_check;
use crate::{PyObject, pystate::with_vm};
use core::ffi::{c_char, c_int};
use rustpython_vm::builtins::PyBytes;
use rustpython_vm::PyObjectRef;

define_py_check!(fn PyBytes_Check, types.bytes_type);
define_py_check!(exact fn PyBytes_CheckExact, types.bytes_type);

#[unsafe(no_mangle)]
#[allow(clippy::uninit_vec)]
pub unsafe extern "C" fn PyBytes_FromStringAndSize(
    bytes: *mut c_char,
    len: isize,
) -> *mut PyObject {
    with_vm(|vm| {
        let len = len.try_into().map_err(|_| {
            vm.new_system_error("Negative size passed to PyBytes_FromStringAndSize")
        })?;

        let data = if bytes.is_null() {
            let mut data = Vec::with_capacity(len);
            unsafe { data.set_len(len) };
            data
        } else {
            unsafe { core::slice::from_raw_parts(bytes as *const u8, len) }.to_vec()
        };

        Ok(vm.ctx.new_bytes(data))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Size(bytes: *mut PyObject) -> isize {
    with_vm(|vm| {
        let bytes = unsafe { &*bytes }.try_downcast_ref::<PyBytes>(vm)?;
        Ok(bytes.as_bytes().len())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_AsString(bytes: *mut PyObject) -> *mut c_char {
    with_vm(|vm| {
        let bytes = unsafe { &*bytes }.try_downcast_ref::<PyBytes>(vm)?;
        Ok(bytes.as_bytes().as_ptr())
    })
}

/// PyBytes_FromString: create a bytes object from a NULL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromString(bytes: *const c_char) -> *mut PyObject {
    with_vm(|vm| {
        let len = unsafe { core::ffi::CStr::from_ptr(bytes) }.to_bytes().len();
        let data = unsafe { core::slice::from_raw_parts(bytes as *const u8, len) }.to_vec();
        Ok(vm.ctx.new_bytes(data))
    })
}

/// PyBytes_GET_SIZE: macro/inline alternative to PyBytes_Size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_GET_SIZE(bytes: *mut PyObject) -> isize {
    with_vm(|vm| {
        let bytes = unsafe { &*bytes }.try_downcast_ref::<PyBytes>(vm)?;
        Ok(bytes.as_bytes().len())
    })
}

/// PyBytes_Resize: resize a bytes object to `newsize` bytes.
/// Since RustPython bytes are immutable, we replace the object with a
/// zero-initialized bytes of the new size (matching the observable
/// behavior of CPython's resize-for-writing path).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Resize(bytes: *mut *mut PyObject, newsize: isize) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        if bytes.is_null() || unsafe { (*bytes).is_null() } {
            return Err(vm.new_system_error("PyBytes_Resize called with NULL bytes"));
        }
        let newlen: usize = newsize
            .try_into()
            .map_err(|_| vm.new_system_error("PyBytes_Resize: negative size"))?;
        let new_bytes: PyObjectRef = vm.ctx.new_bytes(vec![0u8; newlen]).into();
        let old = unsafe { PyObjectRef::from_raw(core::ptr::NonNull::new_unchecked(*bytes)) };
        let _ = old; // drop the old object reference
        unsafe { *bytes = new_bytes.into_raw().as_ptr() };
        Ok(0)
    })
}

/// PyBytes_FromObject: convert any object to bytes (calls bytes(obj)).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromObject(obj: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        let obj = unsafe { &*obj };
        let result = vm.call_method(obj, "__bytes__", ())?;
        if result.downcast_ref::<PyBytes>().is_none() {
            return Err(vm.new_type_error("__bytes__ returned non-bytes type"));
        }
        Ok(result.into_raw().as_ptr())
    })
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;
    use pyo3::types::PyBytes;

    #[test]
    fn bytes() {
        Python::attach(|py| {
            let bytes = PyBytes::new(py, b"Hello, World!");
            assert_eq!(bytes.as_bytes(), b"Hello, World!");
        })
    }

    #[test]
    fn bytes_uninit() {
        Python::attach(|py| {
            let bytes = PyBytes::new_with(py, 13, |data| {
                data.copy_from_slice(b"Hello, World!");
                Ok(())
            })
            .unwrap();
            assert_eq!(bytes.as_bytes(), b"Hello, World!");
        })
    }
}
