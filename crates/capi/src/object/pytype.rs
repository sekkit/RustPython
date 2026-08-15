use crate::methodobject::{PyMethodDef, build_method_def};
use crate::moduleobject::{PySlot, Py_slot_end, Py_slot_invalid};
use crate::object::define_py_check;
use crate::pystate::with_vm;
use crate::util::CStrExt;
use core::ffi::{CStr, c_char, c_int, c_ulong, c_void};
use rustpython_vm::builtins::{PyStr, PyType};
use rustpython_vm::function::FuncArgs;
use rustpython_vm::{AsObject, Py, PyObject, PyObjectRef, VirtualMachine};

pub type PyTypeObject = Py<PyType>;

// CPython 3.15 (PEP 793) type slot ids.
// Names intentionally mirror the C identifiers.
#[allow(non_upper_case_globals, dead_code, unreachable_pub)]
mod slots {
    use core::ffi::c_int;

    pub const Py_tp_base: c_int = 48;
    pub const Py_tp_doc: c_int = 56;
    pub const Py_tp_methods: c_int = 64;
    pub const Py_tp_name: c_int = 95;
    pub const Py_tp_metaclass: c_int = 107;
}
use slots::*;

define_py_check!(fn PyType_Check, types.type_type);
define_py_check!(exact fn PyType_CheckExact, types.type_type);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_TYPE(op: *mut PyObject) -> *const PyTypeObject {
    unsafe { (*op).class() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IS_TYPE(op: *mut PyObject, ty: *mut PyTypeObject) -> c_int {
    with_vm(|_vm| {
        let obj = unsafe { &*op };
        let ty = unsafe { &*ty };
        obj.class().is(ty)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetFlags(ptr: *const PyTypeObject) -> c_ulong {
    let ty = unsafe { &*ptr };
    ty.slots.flags.bits() as u32 as c_ulong
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_IsSubtype(a: *const PyTypeObject, b: *const PyTypeObject) -> c_int {
    with_vm(move |_vm| {
        let a = unsafe { &*a };
        let b = unsafe { &*b };
        Ok(a.is_subtype(b))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetName(ptr: *const PyTypeObject) -> *mut PyObject {
    with_vm(|vm| unsafe { &*ptr }.__name__(vm))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetQualName(ptr: *const PyTypeObject) -> *mut PyObject {
    with_vm(|vm| unsafe { &*ptr }.__qualname__(vm))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleName(ptr: *const PyTypeObject) -> *mut PyObject {
    with_vm(|vm| unsafe { &*ptr }.__module__(vm))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetFullyQualifiedName(ptr: *const PyTypeObject) -> *mut PyObject {
    with_vm(|vm| {
        let ty = unsafe { &*ptr };
        let qualname = ty.__qualname__(vm).try_downcast::<PyStr>(vm)?;
        let module = ty.__module__(vm);

        if let Some(module) = module.downcast_ref::<PyStr>()
            && module.as_wtf8() != "builtins"
        {
            Ok(vm.ctx.new_str(format!("{module}.{qualname}")))
        } else {
            Ok(qualname)
        }
    })
}

/// PyType_GetSlot: read a type slot (CPython 3.15 slot ids). Slots that the
/// vm does not model return NULL, which is the "slot not defined" answer.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetSlot(
    ty: *mut PyTypeObject,
    slot: c_int,
) -> *mut c_void {
    with_vm(|_vm| {
        let ty = unsafe { &*ty };
        match slot {
            Py_tp_base => Ok(ty.base.to_owned().map_or(core::ptr::null_mut(), |b| {
                b.as_object().as_raw().cast_mut().cast()
            })),
            _ => Ok(core::ptr::null_mut()),
        }
    })
}

/// PyType_Freeze: no-op for RustPython (types are never unloaded).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Freeze(_tp: *mut PyTypeObject) -> c_int {
    0
}

/// PyType_FromSlots: build a heap type from a PEP 793 PySlot array (3.15).
/// Supports Py_tp_name, Py_tp_base, Py_tp_metaclass, Py_tp_methods and
/// Py_tp_doc; the type is created through the interpreter's type machinery.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromSlots(slots: *mut PySlot) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        if slots.is_null() {
            return Err(vm.new_system_error("PyType_FromSlots called with NULL slots"));
        }
        let mut name: Option<String> = None;
        let mut base: Option<PyObjectRef> = None;
        let mut metaclass: Option<PyObjectRef> = None;
        let mut methods: *const PyMethodDef = core::ptr::null();
        let mut doc: Option<String> = None;

        let mut i = 0usize;
        loop {
            let slot = unsafe { &*slots.add(i) };
            match slot.sl_id as c_int {
                Py_slot_end => break,
                Py_slot_invalid => {
                    return Err(vm.new_system_error("PyType_FromSlots: invalid slot"));
                }
                Py_tp_name => {
                    let ptr = unsafe { slot.sl_value.sl_ptr }.cast::<c_char>();
                    name = Some(
                        unsafe { CStr::from_ptr(ptr) }
                            .to_str()
                            .map_err(|_| vm.new_system_error("invalid Py_tp_name"))?
                            .to_owned(),
                    );
                }
                Py_tp_base => {
                    base = Some(
                        unsafe { &*slot.sl_value.sl_ptr.cast::<PyObject>() }.to_owned(),
                    );
                }
                Py_tp_metaclass => {
                    metaclass = Some(
                        unsafe { &*slot.sl_value.sl_ptr.cast::<PyObject>() }.to_owned(),
                    );
                }
                Py_tp_methods => {
                    methods = unsafe { slot.sl_value.sl_ptr }.cast::<PyMethodDef>();
                }
                Py_tp_doc => {
                    let ptr = unsafe { slot.sl_value.sl_ptr }.cast::<c_char>();
                    doc = Some(
                        unsafe { CStr::from_ptr(ptr) }
                            .to_str()
                            .map_err(|_| vm.new_system_error("invalid Py_tp_doc"))?
                            .to_owned(),
                    );
                }
                _ => {}
            }
            i += 1;
        }
        let name = name.ok_or_else(|| vm.new_system_error("PyType_FromSlots: missing Py_tp_name"))?;
        let base = base.unwrap_or_else(|| vm.ctx.types.object_type.to_owned().into());

        let dict = vm.ctx.new_dict();
        if !methods.is_null() {
            let count = unsafe { method_def_count(vm, methods)? };
            let mds = unsafe { core::slice::from_raw_parts(methods, count) };
            for md in mds {
                let method = build_method_def(vm, md, false)?.build_function(vm, None);
                let mname = unsafe { md.ml_name.try_as_str(vm) }?;
                dict.set_item(mname, method.into(), vm).map_err(|e| {
                    vm.new_system_error(format!(
                        "PyType_FromSlots: cannot add method {mname}: {}",
                        e.args()
                            .first()
                            .and_then(|a| a.downcast_ref::<PyStr>().map(|s| s.to_string()))
                            .unwrap_or_else(|| "unknown error".to_string())
                    ))
                })?;
            }
        }
        if let Some(doc) = doc {
            dict.set_item(
                rustpython_vm::identifier!(vm, __doc__),
                vm.ctx.new_str(doc).into(),
                vm,
            )?;
        }

        let bases = vm.ctx.new_tuple(vec![base]);
        let metaclass = metaclass.unwrap_or_else(|| vm.ctx.types.type_type.to_owned().into());
        let args = FuncArgs::from(vec![
            vm.ctx.new_str(name).into(),
            bases.into(),
            dict.into(),
        ]);
        metaclass.call(args, vm)
    })
}

unsafe fn method_def_count(
    vm: &VirtualMachine,
    methods: *const PyMethodDef,
) -> rustpython_vm::PyResult<usize> {
    let mut n = 0;
    loop {
        let md = unsafe { &*methods.add(n) };
        if md.ml_name.is_null() {
            return Ok(n);
        }
        if n > 10_000 {
            return Err(vm.new_system_error("PyMethodDef table is not NUL-terminated"));
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;
    use pyo3::types::{PyInt, PyString, PyTypeMethods};

    #[test]
    fn type_name() {
        Python::attach(|py| {
            let string = PyString::new(py, "Hello, World!");
            assert_eq!(string.get_type().name().unwrap().to_str().unwrap(), "str");
        })
    }

    #[test]
    fn type_get_module_name() {
        Python::attach(|py| {
            assert_eq!(
                py.get_type::<PyInt>().module().unwrap().to_str().unwrap(),
                "builtins"
            );
        })
    }
}
