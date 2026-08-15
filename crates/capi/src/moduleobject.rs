use crate::PyObject;
use crate::methodobject::{PyMethodDef, build_method_def};
use crate::object::define_py_check;
use crate::pystate::with_vm;
use crate::util::CStrExt;
use core::ffi::{CStr, c_char, c_int, c_void};
use rustpython_vm::builtins::{PyModule, PyStr, PyTuple};
use rustpython_vm::{AsObject, PyObjectRef, PyResult, VirtualMachine};

define_py_check!(fn PyModule_Check, types.module_type);
define_py_check!(exact fn PyModule_CheckExact, types.module_type);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetNameObject(module: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let module = unsafe { &*module }.try_downcast_ref::<PyModule>(vm)?;
        let dict = module.dict();
        let name = dict
            .get_item_opt(rustpython_vm::identifier!(vm, __name__), vm)?
            .and_then(|obj| obj.downcast_ref::<PyStr>().map(ToOwned::to_owned));
        name.ok_or_else(|| vm.new_system_error("nameless module"))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetFilenameObject(module: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let module = unsafe { &*module }.try_downcast_ref::<PyModule>(vm)?;
        let dict = module.dict();
        let filename = dict
            .get_item_opt(rustpython_vm::identifier!(vm, __file__), vm)?
            .and_then(|obj| obj.downcast_ref::<PyStr>().map(ToOwned::to_owned));
        filename.ok_or_else(|| vm.new_system_error("module filename missing"))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_NewObject(name: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        let name = unsafe { &*name }.try_downcast_ref::<PyStr>(vm)?;
        let name = name
            .to_str()
            .ok_or_else(|| vm.new_system_error("module name must be valid UTF-8"))?;
        Ok(vm.new_module(name, vm.ctx.new_dict(), None))
    })
}

// ---------------------------------------------------------------------------
// PyModuleDef / PyModule_Create (CPython Modules/moduleobject.c, import.c)
//
// Layouts mirror CPython's Include/cpython/moduleobject.h exactly so that
// extensions compiled against CPython headers (including pyo3 abi3 output)
// can pass their PyModuleDef to us.
//
// CPython 3.15 (PEP 793) renumbered the slot ids: module slots now live in
// the 84..110 range instead of the legacy 1..3, and multi-phase modules are
// built from PySlot arrays via PyModule_FromSlotsAndSpec.
// ---------------------------------------------------------------------------

/// PyObject_HEAD is opaque here: extensions allocate the base with CPython's
/// header, and we only touch fields at CPython offsets.
#[repr(C)]
pub struct PyModuleDefBase {
    pub ob_head: [usize; 2], // ob_refcnt, ob_type (PyObject_HEAD)
    pub m_init: Option<unsafe extern "C" fn() -> *mut PyObject>,
    pub m_index: isize,
    pub m_copy: *mut PyObject,
}

#[repr(C)]
pub struct PyModuleDef_Slot {
    pub slot: c_int,
    pub value: *mut c_void,
}

#[repr(C)]
pub struct PyModuleDef {
    pub m_base: PyModuleDefBase,
    pub m_name: *const c_char,
    pub m_doc: *const c_char,
    pub m_size: isize,
    pub m_methods: *const PyMethodDef,
    pub m_slots: *const PyModuleDef_Slot,
    pub m_traverse:
        Option<unsafe extern "C" fn(*mut PyObject, *mut c_void, *mut c_void) -> c_int>,
    pub m_clear: Option<unsafe extern "C" fn(*mut PyObject) -> c_int>,
    pub m_free: Option<unsafe extern "C" fn(*mut PyObject)>,
}

// CPython 3.15 (PEP 793) slot ids.
// Names intentionally mirror the C identifiers.
#[allow(non_upper_case_globals, dead_code, unreachable_pub)]
mod slot_ids {
    use core::ffi::c_int;

    pub const Py_mod_create: c_int = 84;
    pub const Py_mod_exec: c_int = 85;
    pub const Py_mod_multiple_interpreters: c_int = 86;
    pub const Py_mod_gil: c_int = 87;
    pub const Py_mod_name: c_int = 100;
    pub const Py_mod_doc: c_int = 101;
    pub const Py_mod_state_size: c_int = 102;
    pub const Py_mod_methods: c_int = 103;
    pub const Py_mod_abi: c_int = 109;
    pub const Py_mod_token: c_int = 110;
    pub const Py_slot_end: c_int = 0;
    pub const Py_slot_invalid: c_int = 0xffff;
}

use slot_ids::*;
pub use slot_ids::{Py_slot_end, Py_slot_invalid};

/// PEP 793 PySlot: the 3.15 replacement for PyModuleDef_Slot arrays. The
/// layout matches CPython's Include/cpython/slots.h.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct PySlot {
    pub sl_id: u16,
    pub sl_flags: u16,
    pub sl_reserved: u32,
    pub sl_value: PySlotValue,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union PySlotValue {
    pub sl_ptr: *mut c_void,
    pub sl_func: Option<unsafe extern "C" fn()>,
    pub sl_size: isize,
    pub sl_int64: i64,
    pub sl_uint64: u64,
}

static MODULE_INDEX: core::sync::atomic::AtomicIsize =
    core::sync::atomic::AtomicIsize::new(0);

/// PyModuleDef_Init: assign a unique m_index and return the def as an object.
/// 3.15 initializes m_index to 0 (PyModuleDef_HEAD_INIT); older headers used -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModuleDef_Init(def: *mut PyModuleDef) -> *mut PyObject {
    if def.is_null() {
        return core::ptr::null_mut();
    }
    let def = unsafe { &mut *def };
    if def.m_base.m_index == -1 || def.m_base.m_index == 0 {
        def.m_base.m_index = MODULE_INDEX.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    }
    def as *mut PyModuleDef as *mut PyObject
}

/// PyModule_Create: build a module from a PyModuleDef (methods + docstring).
/// As in CPython 3.15, defs with m_slots are rejected; multi-phase modules
/// must go through PyModule_FromSlotsAndSpec instead.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_Create(def: *mut PyModuleDef) -> *mut PyObject {
    unsafe { _PyModule_Create(def) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyModule_Create(def: *mut PyModuleDef) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        if def.is_null() {
            return Err(vm.new_system_error("PyModule_Create called with a null def"));
        }
        if unsafe { PyModuleDef_Init(def) }.is_null() {
            return Err(vm.new_system_error("PyModuleDef_Init failed"));
        }
        let def = unsafe { &*def };
        if !def.m_slots.is_null() {
            return Err(vm.new_system_error(format!(
                "module {}: PyModule_Create is incompatible with m_slots",
                unsafe { def.m_name.try_as_str(vm) }?
            )));
        }
        let name = unsafe { def.m_name.try_as_str(vm) }?;
        let module: PyObjectRef = vm.new_module(name, vm.ctx.new_dict(), None).into();

        add_module_methods(vm, &module, def.m_methods)?;

        if !def.m_doc.is_null() {
            let doc = unsafe { def.m_doc.try_as_str(vm) }?;
            module
                .downcast_ref::<PyModule>()
                .ok_or_else(|| vm.new_system_error("module object is not a module"))?
                .dict()
                .set_item(
                    rustpython_vm::identifier!(vm, __doc__),
                    vm.ctx.new_str(doc).into(),
                    vm,
                )?;
        }

        // Remember the def so PyModule_Exec can find its Py_mod_exec slot.
        store_module_slots(vm, &module, 0, def as *const PyModuleDef as usize)?;

        Ok(module)
    })
}

/// Key under which the C-API stores PEP 793 slot state (exec function and
/// token) on the module's __dict__: RustPython has no per-module C state slot.
const CAPI_SLOTS_KEY: &str = "\0_rustpython_capi_slots";

fn store_module_slots(
    vm: &VirtualMachine,
    module: &PyObjectRef,
    exec_fn: usize,
    token: usize,
) -> PyResult<()> {
    let dict = module
        .downcast_ref::<PyModule>()
        .ok_or_else(|| vm.new_system_error("module object is not a module"))?
        .dict();
    let slots = vm.ctx.new_tuple(vec![
        vm.ctx.new_int(exec_fn as i64).into(),
        vm.ctx.new_int(token as i64).into(),
    ]);
    dict.set_item(&*vm.ctx.new_str(CAPI_SLOTS_KEY), slots.into(), vm)
}

fn read_module_slots(vm: &VirtualMachine, module: &PyObjectRef) -> PyResult<(usize, usize)> {
    let dict = module
        .downcast_ref::<PyModule>()
        .ok_or_else(|| vm.new_system_error("module object is not a module"))?
        .dict();
    let Some(slots) = dict.get_item_opt(&*vm.ctx.new_str(CAPI_SLOTS_KEY), vm)? else {
        return Ok((0, 0));
    };
    let slots = slots
        .downcast_ref::<PyTuple>()
        .ok_or_else(|| vm.new_system_error("invalid C-API module slots state"))?;
    let items = slots.as_slice();
    let exec_fn = items
        .first()
        .and_then(|o| o.downcast_ref::<rustpython_vm::builtins::PyInt>())
        .and_then(|i| i.as_bigint().try_into().ok())
        .unwrap_or(0usize);
    let token = items
        .get(1)
        .and_then(|o| o.downcast_ref::<rustpython_vm::builtins::PyInt>())
        .and_then(|i| i.as_bigint().try_into().ok())
        .unwrap_or(0usize);
    Ok((exec_fn, token))
}

fn add_module_methods(
    vm: &VirtualMachine,
    module: &PyObjectRef,
    methods: *const PyMethodDef,
) -> PyResult<()> {
    if methods.is_null() {
        return Ok(());
    }
    let method_count = unsafe { method_def_count(methods) };
    let methods = unsafe { core::slice::from_raw_parts(methods, method_count) };
    let dict = module
        .downcast_ref::<PyModule>()
        .ok_or_else(|| vm.new_system_error("module object is not a module"))?
        .dict();
    for md in methods {
        let method = build_method_def(vm, md, false)?.build_function(vm, None);
        let name = unsafe { md.ml_name.try_as_str(vm) }?;
        dict.set_item(name, method.into(), vm).map_err(|e| {
            let msg = e
                .args()
                .first()
                .and_then(|a| a.downcast_ref::<PyStr>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown error".to_string());
            vm.new_system_error(format!("cannot add method: {msg}"))
        })?;
    }
    Ok(())
}

/// PyModule_FromSlotsAndSpec: build a module from a PEP 793 PySlot array and
/// a module spec object (CPython 3.15).
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_FromSlotsAndSpec(
    slots: *const PySlot,
    spec: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        if slots.is_null() {
            return Err(vm.new_system_error(
                "PyModule_FromSlotsAndSpec called with NULL slots",
            ));
        }
        let spec = unsafe { &*spec }.to_owned();
        let name_obj = spec.get_attr("name", vm)?;
        let name = name_obj
            .downcast_ref::<PyStr>()
            .ok_or_else(|| vm.new_system_error("module spec has no str name attribute"))?
            .to_string();

        let mut create: Option<
            unsafe extern "C" fn(*mut PyObject, *mut PyModuleDef) -> *mut PyObject,
        > = None;
        let mut exec_fn: Option<unsafe extern "C" fn(*mut PyObject) -> c_int> = None;
        let mut token: usize = 0;
        let mut m_name: Option<String> = None;
        let mut m_doc: Option<String> = None;
        let mut m_methods: *const PyMethodDef = core::ptr::null();
        let mut saw_abi = false;

        let mut i = 0usize;
        loop {
            let slot = unsafe { &*slots.add(i) };
            match slot.sl_id as c_int {
                Py_slot_end => break,
                Py_slot_invalid => {
                    return Err(vm.new_system_error(format!(
                        "module {name} contains an invalid slot"
                    )));
                }
                Py_mod_create => {
                    create = unsafe {
                        core::mem::transmute::<
                            Option<unsafe extern "C" fn()>,
                            Option<unsafe extern "C" fn(*mut PyObject, *mut PyModuleDef) -> *mut PyObject>,
                        >(slot.sl_value.sl_func)
                    };
                }
                Py_mod_exec => {
                    exec_fn = unsafe {
                        core::mem::transmute::<
                            Option<unsafe extern "C" fn()>,
                            Option<unsafe extern "C" fn(*mut PyObject) -> c_int>,
                        >(slot.sl_value.sl_func)
                    };
                }
                Py_mod_multiple_interpreters | Py_mod_gil => {}
                Py_mod_abi => {
                    saw_abi = true;
                }
                Py_mod_token => {
                    token = unsafe { slot.sl_value.sl_ptr } as usize;
                }
                Py_mod_name => {
                    let ptr = unsafe { slot.sl_value.sl_ptr }.cast::<c_char>();
                    m_name = Some(
                        unsafe { CStr::from_ptr(ptr) }
                            .to_str()
                            .map_err(|_| vm.new_system_error("invalid Py_mod_name"))?
                            .to_owned(),
                    );
                }
                Py_mod_doc => {
                    let ptr = unsafe { slot.sl_value.sl_ptr }.cast::<c_char>();
                    m_doc = Some(
                        unsafe { CStr::from_ptr(ptr) }
                            .to_str()
                            .map_err(|_| vm.new_system_error("invalid Py_mod_doc"))?
                            .to_owned(),
                    );
                }
                Py_mod_state_size => {}
                Py_mod_methods => {
                    m_methods = unsafe { slot.sl_value.sl_ptr }.cast::<PyMethodDef>();
                }
                _ => {}
            }
            i += 1;
        }
        let _ = m_name;
        if !saw_abi {
            return Err(vm.new_system_error(format!(
                "module {name} does not define Py_mod_abi, which is mandatory \
                 for modules defined from slots only."
            )));
        }

        let module: PyObjectRef = if let Some(create) = create {
            let created =
                unsafe { create(spec.as_object().as_raw().cast_mut(), core::ptr::null_mut()) };
            if created.is_null() {
                return Err(vm.new_system_error(format!(
                    "creation of module {name} failed without setting an exception"
                )));
            }
            unsafe { &*created }.to_owned()
        } else {
            vm.new_module(&name, vm.ctx.new_dict(), None).into()
        };

        add_module_methods(vm, &module, m_methods)?;

        if let Some(doc) = m_doc {
            module
                .downcast_ref::<PyModule>()
                .ok_or_else(|| vm.new_system_error("module object is not a module"))?
                .dict()
                .set_item(
                    rustpython_vm::identifier!(vm, __doc__),
                    vm.ctx.new_str(doc).into(),
                    vm,
                )?;
        }

        store_module_slots(vm, &module, exec_fn.map_or(0, |f| f as usize), token)?;

        Ok(module)
    })
}

/// PyModule_Exec: run the module's Py_mod_exec function (stored by
/// PyModule_FromSlotsAndSpec / _PyModule_Create).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_Exec(module: *mut PyObject) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        let module = unsafe { &*module }.to_owned();
        let (exec_fn, token) = read_module_slots(vm, &module)?;
        if exec_fn != 0 {
            let exec: unsafe extern "C" fn(*mut PyObject) -> c_int =
                unsafe { core::mem::transmute(exec_fn) };
            let rc = unsafe { exec(module.as_object().as_raw().cast_mut()) };
            if rc != 0 {
                return match vm.take_raised_exception() {
                    Some(exc) => Err(exc),
                    None => Err(vm.new_system_error(
                        "execution of module failed without setting an exception",
                    )),
                };
            }
            return Ok(0);
        }
        if token != 0 {
            // Created from a PyModuleDef: run its Py_mod_exec slot.
            let def = token as *const PyModuleDef;
            return Ok(unsafe {
                PyModule_ExecDef(module.as_object().as_raw().cast_mut(), def as *mut _)
            });
        }
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_ExecDef(
    module: *mut PyObject,
    def: *mut PyModuleDef,
) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        let def = unsafe { &*def };
        if def.m_slots.is_null() {
            return Ok(0);
        }
        let module = unsafe { &*module }.to_owned();
        let slot_count = unsafe { module_slot_count(def.m_slots) };
        let slots = unsafe { core::slice::from_raw_parts(def.m_slots, slot_count) };
        for slot in slots {
            if slot.slot == Py_mod_exec {
                let exec: unsafe extern "C" fn(*mut PyObject) -> c_int =
                    unsafe { core::mem::transmute(slot.value) };
                let rc = unsafe { exec(module.as_object().as_raw().cast_mut()) };
                if rc != 0 {
                    return match vm.take_raised_exception() {
                        Some(exc) => Err(exc),
                        None => Err(vm.new_system_error(
                            "execution of module failed without setting an exception",
                        )),
                    };
                }
            }
        }
        Ok(0)
    })
}

/// PyModule_SetDocString: set the module's __doc__ attribute (3.13+).
/// A NULL doc is a bad internal call, as in CPython.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_SetDocString(
    module: *mut PyObject,
    doc: *const c_char,
) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        let doc = unsafe { doc.try_as_str(vm)? };
        let module = unsafe { &*module }.to_owned();
        module.set_attr(rustpython_vm::identifier!(vm, __doc__), vm.ctx.new_str(doc), vm)?;
        Ok(0)
    })
}

/// PyModule_GetDict: return the module's __dict__ (borrowed reference).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetDict(module: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let module = unsafe { &*module }
            .try_downcast_ref::<PyModule>(vm)
            .map_err(|_| vm.new_system_error("Bad internal call"))?;
        let dict: PyObjectRef = module.dict().into();
        let ptr = dict.as_object().as_raw().cast_mut();
        // Borrowed reference: keep the refcount unchanged.
        core::mem::forget(dict);
        Ok(ptr)
    })
}

unsafe fn method_def_count(methods: *const PyMethodDef) -> usize {
    let mut n = 0;
    loop {
        let md = unsafe { &*methods.add(n) };
        if md.ml_name.is_null() {
            return n;
        }
        n += 1;
    }
}

unsafe fn module_slot_count(slots: *const PyModuleDef_Slot) -> usize {
    let mut n = 0;
    loop {
        let slot = unsafe { &*slots.add(n) };
        if slot.slot == 0 {
            return n;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::methodobject::PyMethodPointer;
    use crate::pystate::with_vm;
    use core::ffi::c_int;
    use pyo3::prelude::*;
    use rustpython_vm::function::PyMethodFlags;

    unsafe extern "C" fn hello_fn(_slf: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
        with_vm(|vm| -> rustpython_vm::PyResult<rustpython_vm::PyObjectRef> {
            Ok(vm.ctx.new_str("hello from C!").into())
        })
    }

    #[test]
    fn module_create_with_methods() {
        let mut method = PyMethodDef {
            ml_name: c"hello".as_ptr(),
            ml_meth: PyMethodPointer {
                PyCFunction: hello_fn,
            },
            ml_flags: PyMethodFlags::NOARGS.bits() as c_int,
            ml_doc: c"say hello".as_ptr(),
        };
        let mut def = PyModuleDef {
            m_base: PyModuleDefBase {
                ob_head: [0; 2],
                m_init: None,
                m_index: -1,
                m_copy: core::ptr::null_mut(),
            },
            m_name: c"testmod".as_ptr(),
            m_doc: core::ptr::null(),
            m_size: 0,
            m_methods: core::ptr::null(),
            m_slots: core::ptr::null(),
            m_traverse: None,
            m_clear: None,
            m_free: None,
        };
        Python::attach(|py| {
            unsafe {
                def.m_methods = &mut method as *mut PyMethodDef;
                let module_ptr = PyModule_Create(&mut def as *mut PyModuleDef);
                assert!(!module_ptr.is_null(), "PyModule_Create returned NULL");
                let module: Py<pyo3::PyAny> =
                    Py::from_owned_ptr(py, module_ptr as *mut pyo3::ffi::PyObject);
                let name: String = module.getattr(py, "__name__").unwrap().extract(py).unwrap();
                assert_eq!(name, "testmod");
                let hello = module.getattr(py, "hello").unwrap();
                let result: String = hello.call0(py).unwrap().extract(py).unwrap();
                assert_eq!(result, "hello from C!");
            }
        });
    }

    #[test]
    fn module_def_init_assigns_index() {
        let mut def = PyModuleDef {
            m_base: PyModuleDefBase {
                ob_head: [0; 2],
                m_init: None,
                m_index: -1,
                m_copy: core::ptr::null_mut(),
            },
            m_name: c"idxmod".as_ptr(),
            m_doc: core::ptr::null(),
            m_size: 0,
            m_methods: core::ptr::null(),
            m_slots: core::ptr::null(),
            m_traverse: None,
            m_clear: None,
            m_free: None,
        };
        Python::attach(|_py| {
            unsafe {
                let ptr = PyModuleDef_Init(&mut def as *mut PyModuleDef);
                assert!(!ptr.is_null());
                assert_ne!(def.m_base.m_index, -1);
            }
        });
    }
}
