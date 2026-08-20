use crate::get_main_interpreter;
use crate::pyerrors::init_exception_statics;
use crate::pystate::{ensure_thread_has_vm_attached, with_vm};
use crate::util::CStrExt;
use crate::PyObject;
use alloc::ffi::CString;
use core::ffi::{c_char, c_int, c_ulong};
use core::sync::atomic::{AtomicPtr, Ordering};
use rustpython_vm::common::rc::PyRc;
use rustpython_vm::stdlib::sys;
use rustpython_vm::version::{MAJOR, MICRO, MINOR, RUSTPYTHON_BUILD_INFO, VERSION_HEX};
use rustpython_vm::vm::thread::ThreadedVirtualMachine;
use rustpython_vm::{Context, Interpreter, PyResult};
use std::sync::{LazyLock, Mutex};

pub(crate) static MAIN_INTERP: Mutex<Option<Interpreter>> = Mutex::new(None);
pub(crate) static MAIN_INTERP_PTR: AtomicPtr<Interpreter> = AtomicPtr::new(core::ptr::null_mut());

/// Request a thread local vm from the main interpreter
pub(crate) fn request_vm_from_interpreter() -> ThreadedVirtualMachine {
    get_main_interpreter()
        .as_ref()
        .expect("Interpreter not initialized")
        .enter(|vm| vm.new_thread())
}

#[unsafe(no_mangle)]
pub static Py_Version: c_ulong = VERSION_HEX as c_ulong;

#[unsafe(no_mangle)]
pub extern "C" fn Py_IsInitialized() -> c_int {
    !MAIN_INTERP_PTR.load(Ordering::Acquire).is_null() as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_Initialize() {
    Py_InitializeEx(0);
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_InitializeEx(_initsigs: c_int) {
    let mut interp = get_main_interpreter();
    if interp.is_none() {
        // Safety: Interpreter was not initialized before, so we can safely assume the statics are not used
        unsafe { init_exception_statics(&Context::genesis().exceptions) };
        let builder = Interpreter::builder(Default::default());
        let defs = rustpython_stdlib::stdlib_module_defs(&builder.ctx);
        *interp = builder
            .add_native_modules(&defs)
            .init_hook(|vm| {
                let state = PyRc::get_mut(&mut vm.state).unwrap();
                let path = rustpython_pylib::LIB_PATH.to_owned();

                state.config.paths.stdlib_dir = Some(path.clone());
                state.config.paths.module_search_paths.insert(0, path);
            })
            .build()
            .into();
        MAIN_INTERP_PTR.store(
            interp.as_ref().unwrap() as *const _ as *mut _,
            Ordering::Release,
        );
        drop(interp);
        ensure_thread_has_vm_attached();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_Finalize() {
    let _ = Py_FinalizeEx();
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_FinalizeEx() -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_IsFinalizing() -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_GetVersion() -> *const c_char {
    static VERSION: LazyLock<CString> = LazyLock::new(|| {
        CString::new(format!("{MAJOR}.{MINOR}.{MICRO}"))
            .expect("version string must not contain interior NULs")
    });
    VERSION.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_GetBuildInfo() -> *const c_char {
    static BUILD_INFO: LazyLock<CString> = LazyLock::new(|| {
        CString::new(RUSTPYTHON_BUILD_INFO).expect("build info must not contain interior NULs")
    });
    BUILD_INFO.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_GetCompiler() -> *const c_char {
    c"[RUST]".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_GetCopyright() -> *const c_char {
    sys::COPYRIGHT.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_GetPlatform() -> *const c_char {
    sys::PLATFORM.as_ptr()
}

/// Rust implementation of the C shim's Py_Exit.
#[unsafe(no_mangle)]
pub extern "C" fn rp_va_exit(status: c_int) {
    std::process::exit(status);
}

/// Rust implementation of the C shim's PySys_GetObject.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_sys_get_object(name: *const c_char) -> *mut PyObject {
    with_vm(|vm| {
        let name = unsafe { name.try_as_str(vm) }?;
        vm.sys_module.get_attr(name, vm)
    })
}

/// Rust implementation of the C shim's PySys_SetObject.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_sys_set_object(
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    with_vm(|vm| {
        let name = unsafe { name.try_as_str(vm) }?;
        let value = unsafe { &*value }.to_owned();
        vm.sys_module.set_attr(name, value, vm)
    })
}

/// Rust implementation of the C shim's PySys_SetPath.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_sys_set_path(path: *const c_char) {
    with_vm(|vm| {
        if path.is_null() {
            return;
        }
        let path_str = match unsafe { path.try_as_str(vm) } {
            Ok(s) => s,
            Err(_) => return,
        };
        let parts: Vec<&str> = path_str.split(&[':', ';'][..]).collect();
        let path_list: Vec<rustpython_vm::PyObjectRef> = parts
            .iter()
            .map(|p| vm.ctx.new_str(p.to_string()).into())
            .collect();
        if let Err(e) = vm.sys_module.set_attr("path", vm.ctx.new_list(path_list), vm) {
            vm.print_exception(e);
        }
    })
}

/// Rust implementation of the C shim's PyRun_SimpleString.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_run_simple_string(code: *const c_char) -> c_int {
    with_vm(|vm| -> PyResult<c_int> {
        if code.is_null() {
            return Err(vm.new_system_error("PyRun_SimpleString: NULL code"));
        }
        let code_str = unsafe { core::ffi::CStr::from_ptr(code) }
            .to_str()
            .map_err(|_| vm.new_system_error("PyRun_SimpleString: not valid UTF-8"))?;
        let scope = vm.new_scope_with_builtins();
        let res = vm.run_code_string(scope, code_str, "<string>");
        match res {
            Ok(_) => Ok(0),
            Err(exc) => {
                vm.set_exception(Some(exc));
                Ok(-1)
            }
        }
    })
}

/// Rust implementation of the C shim's PyRun_String.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_run_string(
    code: *const c_char,
    _start: c_int,
    _globals: *mut PyObject,
    _locals: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        if code.is_null() {
            return Err(vm.new_system_error("PyRun_String: NULL code"));
        }
        let code_str = unsafe { core::ffi::CStr::from_ptr(code) }
            .to_str()
            .map_err(|_| vm.new_system_error("PyRun_String: not valid UTF-8"))?;
        let scope = vm.new_scope_with_builtins();
        vm.run_code_string(scope, code_str, "<string>")
    })
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;

    #[test]
    fn get_version() {
        Python::attach(|py| {
            let version = py.version_info();
            assert!(version >= (3, 14));
        });

        assert!(unsafe { pyo3::ffi::Py_Version } >= 0x030d0000);
    }
}
