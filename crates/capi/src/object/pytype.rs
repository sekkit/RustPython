use crate::methodobject::{PyMethodDef, build_method_def};
use crate::moduleobject::{Py_slot_end, Py_slot_invalid, PySlot};
use crate::object::define_py_check;
use crate::pystate::with_vm;
use crate::util::CStrExt;
use core::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};
use core::ptr::NonNull;
use rustpython_vm::builtins::{PyStr, PyType};
use rustpython_vm::function::FuncArgs;
use rustpython_vm::protocol::{CBufferSlots, CPyBuffer};
use rustpython_vm::{AsObject, Py, PyObject, PyObjectRef, PyRef, VirtualMachine};

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
    with_vm(move |vm| {
        let a = resolve_type_ptr(vm, a)?;
        let b = resolve_type_ptr(vm, b)?;
        let a: &Py<PyType> = &a;
        let b: &Py<PyType> = &b;
        Ok(a.is_subtype(b))
    })
}

/// The C-visible `ob_type` of a RustPython object (offset 8 of the object
/// header) is the payload vtable, not a type pointer. CPython's inline
/// PyObject_TypeCheck falls back to calling the exported PyType_IsSubtype
/// with those raw pointers, so resolve them: known payload vtables map to
/// their real type; the exported type-stub symbols (byte copies of a type's
/// header) carry the real type's fields and can be used directly.
fn resolve_type_ptr(
    vm: &VirtualMachine,
    ptr: *const PyTypeObject,
) -> rustpython_vm::PyResult<PyRef<PyType>> {
    let addr = ptr as usize;
    if addr == 0 {
        return Err(vm.new_system_error("PyType_IsSubtype called with a NULL type"));
    }
    for (vtable, ty) in vtable_probes(vm) {
        if addr == vtable {
            let obj =
                unsafe { rustpython_vm::PyObjectRef::from_raw(NonNull::new_unchecked(ty.cast())) };
            return obj
                .downcast::<PyType>()
                .map_err(|_| vm.new_system_error("PyType_IsSubtype: vtable probe is not a type"));
        }
    }
    // The exported type-stub symbols only mirror a type's header; map them
    // back to the real types so their full payload (mro, bases) is readable.
    // Accept both the exe's own stubs and the relay's copies (the addresses
    // extensions actually resolve).
    use crate::objectstatics::{StubKind, is_type_stub_addr};
    if is_type_stub_addr(addr, StubKind::Str) {
        return Ok(vm.ctx.types.str_type.to_owned());
    }
    if is_type_stub_addr(addr, StubKind::Int) {
        return Ok(vm.ctx.types.int_type.to_owned());
    }
    if is_type_stub_addr(addr, StubKind::Bool) {
        return Ok(vm.ctx.types.bool_type.to_owned());
    }
    // Not a vtable: assume a real type object (possibly one of our exported
    // header stubs, which mirror the type's header bytes).
    let obj = unsafe { (&*(ptr as *mut PyObject)).to_owned() };
    obj.downcast::<PyType>().map_err(|_| {
        vm.new_system_error("PyType_IsSubtype called with a pointer that is not a type")
    })
}

/// (payload vtable address, real type object pointer) pairs for the common
/// builtin types, captured once from fresh instances.
fn vtable_probes(vm: &VirtualMachine) -> Vec<(usize, *mut u8)> {
    use rustpython_vm::PyPayload;
    use rustpython_vm::builtins::{
        PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyStr, PyTuple,
    };

    fn probe<T: PyPayload>(obj: PyObjectRef, ty: PyRef<PyType>) -> (usize, *mut u8) {
        let vtable = unsafe { *((obj.as_object().as_raw() as *const u8).add(8) as *const usize) };
        (vtable, ty.as_object().as_raw().cast_mut().cast())
    }

    let mut probes = Vec::new();
    let mut push = |obj: PyObjectRef, ty: PyRef<PyType>| probes.push(probe::<PyStr>(obj, ty));
    push(vm.ctx.new_str("").into(), vm.ctx.types.str_type.to_owned());
    probes.push(probe::<PyInt>(
        vm.ctx.new_int(0).into(),
        vm.ctx.types.int_type.to_owned(),
    ));
    probes.push(probe::<PyBool>(
        vm.ctx.new_bool(true).into(),
        vm.ctx.types.bool_type.to_owned(),
    ));
    probes.push(probe::<PyFloat>(
        vm.ctx.new_float(0.0).into(),
        vm.ctx.types.float_type.to_owned(),
    ));
    probes.push(probe::<PyBytes>(
        vm.ctx.new_bytes(vec![]).into(),
        vm.ctx.types.bytes_type.to_owned(),
    ));
    probes.push(probe::<PyTuple>(
        vm.ctx.new_tuple(vec![]).into(),
        vm.ctx.types.tuple_type.to_owned(),
    ));
    probes.push(probe::<PyList>(
        vm.ctx.new_list(vec![]).into(),
        vm.ctx.types.list_type.to_owned(),
    ));
    probes.push(probe::<PyDict>(
        vm.ctx.new_dict().into(),
        vm.ctx.types.dict_type.to_owned(),
    ));
    probes
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
pub unsafe extern "C" fn PyType_GetSlot(ty: *mut PyTypeObject, slot: c_int) -> *mut c_void {
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

// ---------------------------------------------------------------------------
// Legacy (3.14 and earlier) type creation from a PyType_Spec (typeslots.h
// slot ids). Behavioral C slots (getattro/setattr/finalize/traverse/...) are
// not modeled: instances are ordinary Python objects with a __dict__, which
// makes attribute access behave equivalently for well-behaved extensions.
// ---------------------------------------------------------------------------

/// CPython 3.14's PyType_Spec (Include/object.h): three packed ints, so the
/// slots pointer sits at offset 24 (no padding between the ints).
#[repr(C)]
pub struct PyType_Spec {
    pub name: *const c_char,
    pub basicsize: c_int,
    pub itemsize: c_int,
    pub flags: c_uint,
    pub slots: *const PyType_Slot, /* terminated by slot==0. */
}

#[repr(C)]
pub struct PyType_Slot {
    pub slot: c_int,
    pub pfunc: *mut c_void,
}

const _: () = assert!(core::mem::offset_of!(PyType_Spec, slots) == 24);
const _: () = assert!(core::mem::size_of::<PyType_Spec>() == 32);
const _: () = assert!(core::mem::offset_of!(PyType_Slot, pfunc) == 8);
const _: () = assert!(core::mem::size_of::<PyType_Slot>() == 16);

const PY_TP_BASE: c_int = 48;
const PY_TP_DOC: c_int = 56;
const PY_TP_METHODS: c_int = 64;
const PY_BF_GETBUFFER: c_int = 1;
const PY_BF_RELEASEBUFFER: c_int = 2;

fn map_base_ptr(vm: &VirtualMachine, ptr: *mut c_void) -> rustpython_vm::PyResult<PyObjectRef> {
    if ptr.is_null() {
        return Ok(vm.ctx.types.object_type.to_owned().into());
    }
    if crate::objectstatics::is_type_stub_addr(ptr as usize, crate::objectstatics::StubKind::Str) {
        // &PyUnicode_Type: map the exported stub (exe's or relay's) to the
        // real str type.
        return Ok(vm.ctx.types.str_type.to_owned().into());
    }
    let obj = unsafe { (&*(ptr as *mut PyObject)).to_owned() };
    if obj.downcast_ref::<PyType>().is_some() {
        Ok(obj)
    } else {
        Err(vm.new_type_error("PyType_FromSpec: base is not a type"))
    }
}

/// Build a heap type from a PyType_Spec (CPython's PyType_FromSpec).
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromSpec(spec: *mut PyType_Spec) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        if spec.is_null() {
            return Err(vm.new_system_error("PyType_FromSpec called with NULL spec"));
        }
        let spec = unsafe { &*spec };
        let name = unsafe { CStr::from_ptr(spec.name) }
            .to_str()
            .map_err(|_| vm.new_system_error("PyType_FromSpec: invalid type name"))?
            .to_owned();
        let mut base: Option<PyObjectRef> = None;
        let mut methods: *const PyMethodDef = core::ptr::null();
        let mut doc: Option<String> = None;
        let mut c_getbuffer: Option<
            unsafe extern "C" fn(*mut PyObject, *mut CPyBuffer, c_int) -> c_int,
        > = None;
        let mut c_releasebuffer: Option<unsafe extern "C" fn(*mut PyObject, *mut CPyBuffer)> = None;

        let mut i = 0usize;
        loop {
            let slot = unsafe { &*spec.slots.add(i) };
            if slot.slot == 0 {
                break;
            }
            match slot.slot {
                PY_TP_BASE => {
                    base = Some(map_base_ptr(vm, slot.pfunc)?);
                }
                PY_TP_METHODS => {
                    methods = slot.pfunc.cast::<PyMethodDef>();
                }
                PY_TP_DOC => {
                    doc = Some(
                        unsafe { CStr::from_ptr(slot.pfunc.cast::<c_char>()) }
                            .to_str()
                            .map_err(|_| vm.new_system_error("PyType_FromSpec: invalid doc"))?
                            .to_owned(),
                    );
                }
                PY_BF_GETBUFFER => {
                    c_getbuffer = Some(unsafe { core::mem::transmute(slot.pfunc) });
                }
                PY_BF_RELEASEBUFFER => {
                    c_releasebuffer = Some(unsafe { core::mem::transmute(slot.pfunc) });
                }
                // Behavioral slots (getattro/setattr/finalize/traverse/...)
                // are not modeled; the default dict-based behavior is used.
                _ => {}
            }
            i += 1;
        }

        let base = base.unwrap_or_else(|| vm.ctx.types.object_type.to_owned().into());
        let dict = vm.ctx.new_dict();
        if !methods.is_null() {
            let count = unsafe { method_def_count(vm, methods)? };
            let mds = unsafe { core::slice::from_raw_parts(methods, count) };
            for md in mds {
                let method = build_method_def(vm, md, true)?.build_function(vm, None);
                let mname = unsafe { md.ml_name.try_as_str(vm) }?;
                dict.set_item(mname, method.into(), vm).map_err(|e| {
                    vm.new_system_error(format!(
                        "PyType_FromSpec: cannot add method {mname}: {}",
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
        let args = FuncArgs::from(vec![vm.ctx.new_str(name).into(), bases.into(), dict.into()]);
        let metaclass: PyObjectRef = vm.ctx.types.type_type.to_owned().into();
        metaclass.call(args, vm).and_then(|ty| {
            if let Some(getbuffer) = c_getbuffer {
                let ty = ty
                    .downcast_ref::<PyType>()
                    .ok_or_else(|| vm.new_system_error("PyType_FromSpec: result is not a type"))?;
                ty.init_type_data(CBufferSlots {
                    getbuffer,
                    releasebuffer: c_releasebuffer,
                })
                .map_err(|e| vm.new_system_error(e))?;
            }
            Ok(ty)
        })
    })
}

/// PyType_FromModuleAndSpec (3.9+): (module, spec, userdata) — the module
/// context is not modeled, the spec is passed through to PyType_FromSpec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromModuleAndSpec(
    _module: *mut PyObject,
    spec: *mut PyType_Spec,
    _userdata: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyType_FromSpec(spec) }
}

/// PyType_GetModule / PyType_GetModuleState / PyType_GetModuleByDef: heap
/// types are not associated with a C module context, so these return NULL
/// (like CPython does for types without a module association).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModule(_tp: *mut PyTypeObject) -> *mut PyObject {
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleState(_tp: *mut PyTypeObject) -> *mut c_void {
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleByDef(
    _tp: *mut PyTypeObject,
    _def: *mut crate::moduleobject::PyModuleDef,
) -> *mut PyObject {
    core::ptr::null_mut()
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
                    base = Some(unsafe { &*slot.sl_value.sl_ptr.cast::<PyObject>() }.to_owned());
                }
                Py_tp_metaclass => {
                    metaclass =
                        Some(unsafe { &*slot.sl_value.sl_ptr.cast::<PyObject>() }.to_owned());
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
        let name =
            name.ok_or_else(|| vm.new_system_error("PyType_FromSlots: missing Py_tp_name"))?;
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
        let args = FuncArgs::from(vec![vm.ctx.new_str(name).into(), bases.into(), dict.into()]);
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
