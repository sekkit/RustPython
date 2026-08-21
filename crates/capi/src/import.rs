use crate::util::CStrExt;
use crate::{PyObject, pystate::with_vm};
use core::ffi::c_char;
use rustpython_vm::builtins::{PyCode, PyDict, PyModule, PyStr};
use rustpython_vm::import::import_code_obj;
use rustpython_vm::AsObject;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_Import(name: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let name = unsafe { (&*name).try_downcast_ref::<PyStr>(vm)? };
        let _ = vm.import(name, 0)?;

        vm.sys_module
            .get_attr(rustpython_vm::identifier!(vm, modules), vm)?
            .get_item(name, vm)
    })
}

/// PyImport_ImportModule: import a module by C string name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportModule(name: *const c_char) -> *mut PyObject {
    with_vm(|vm| {
        let name = unsafe { name.try_as_str(vm) }?;
        vm.import(name, 0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_AddModuleRef(name: *const c_char) -> *mut PyObject {
    with_vm(|vm| {
        let name = unsafe { name.try_as_str(vm) }?;

        let sys_modules = vm
            .sys_module
            .get_attr(rustpython_vm::identifier!(vm, modules), vm)?;

        sys_modules
            .try_downcast_ref::<PyDict>(vm)?
            .get_item_opt(name, vm)?
            .map_or_else(
                || {
                    let module = vm.new_module(name, vm.ctx.new_dict(), None);
                    sys_modules.set_item(name, module.clone().into(), vm)?;
                    Ok(module)
                },
                |module| {
                    let module = module.try_downcast_ref::<PyModule>(vm)?;
                    Ok(module.to_owned())
                },
            )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ExecCodeModuleEx(
    name: *const c_char,
    co: *mut PyObject,
    pathname: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| {
        let name = unsafe { name.try_as_str(vm) }?;
        let code = unsafe { &*co }.try_downcast_ref::<PyCode>(vm)?;
        let module = import_code_obj(vm, name, code.to_owned(), false)?;

        if let Some(pathname) = unsafe { pathname.try_as_str_opt(vm) }? {
            module.set_attr("__file__", vm.ctx.new_str(pathname), vm)?;
        }

        Ok(module)
    })
}

/// Rust impl of PyImport_ExecCodeModule: import a code object as a module (no pathname).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_import_exec_code_module(
    name: *const c_char,
    co: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyImport_ExecCodeModuleEx(name, co, core::ptr::null()) }
}

/// Rust impl of PyImport_ExecCodeModuleObject: import a code object with PyObject args.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_import_exec_code_module_object(
    name: *mut PyObject,
    co: *mut PyObject,
    pathname: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<*mut PyObject> {
        let name_str = unsafe { &*name }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_type_error("name must be a string")
        })?;
        let code = unsafe { &*co }.try_downcast_ref::<PyCode>(vm)?;
        let module = import_code_obj(vm, name_str, code.to_owned(), false)?;
        if !pathname.is_null() && !unsafe { &*pathname }.is(vm.ctx.none().as_object()) {
            module.set_attr("__file__", unsafe { &*pathname }.to_owned(), vm)?;
        }
        Ok(module.into_raw().as_ptr())
    })
}

/// Rust impl of PyImport_GetImporter: return the importer for a path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_import_get_importer(
    path: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<*mut PyObject> {
        let path_obj = unsafe { &*path }.to_owned();
        // Use importlib._bootstrap._get_importer
        let importlib = vm.import("importlib", 0)?;
        let bootstrap = importlib.get_attr("_bootstrap", vm)?;
        let get_importer = bootstrap.get_attr("_get_importer", vm)?;
        let result = vm.invoke(&get_importer, (path_obj,))?;
        Ok(result.into_raw().as_ptr())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_import_exec_code_module_with_pathnames(
    name: *const c_char,
    co: *mut PyObject,
    pathname: *const c_char,
    orig_pathname: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<*mut PyObject> {
        let name = unsafe { name.try_as_str(vm) }?;
        let code = unsafe { &*co }.try_downcast_ref::<PyCode>(vm)?;
        let module = import_code_obj(vm, name, code.to_owned(), false)?;
        if let Some(pathname) = unsafe { pathname.try_as_str_opt(vm) }? {
            module.set_attr("__file__", vm.ctx.new_str(pathname), vm)?;
        }
        Ok(module.into_raw().as_ptr())
    })
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;

    #[test]
    fn import() {
        Python::attach(|py| {
            let _module = py.import("sys").unwrap();
        })
    }

    #[test]
    fn import_stdlib() {
        Python::attach(|py| {
            let _module = py.import("types").unwrap();
        })
    }

    #[test]
    fn import_sub_module() {
        Python::attach(|py| {
            let module = py.import("collections.abc").unwrap();
            module.getattr("Sequence").unwrap();
        })
    }
}
