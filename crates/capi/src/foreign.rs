//! Slot dispatch for foreign C extension objects.
//!
//! When a C extension allocates an object through `_PyObject_New` it gets a
//! raw buffer whose header matches CPython's `PyObject` (ob_refcnt at offset
//! 0, ob_type at offset 8). Such an object is *foreign*: it is not a live
//! RustPython heap object, so ordinary attribute access cannot reach it.
//! Its behavior lives in the `tp_*` slots of its type, and this module
//! routes calls and attribute lookups into those C function pointers.
//!
//! Two type layouts are recognized:
//!
//! - This repo's CPython-compatible type stubs (see `object::pytype::
//!   alloc_type_stub` and `objectstatics::fill_type_stub`), which squeeze
//!   `tp_repr` next to `tp_str`, shifting the following slots by one word:
//!   tp_call at byte 128, tp_getattro at byte 152, tp_setattro at byte 160.
//! - A foreign extension's own `PyTypeObject` compiled against the standard
//!   CPython headers, where tp_repr sits before the number suites and
//!   tp_getattro lands at byte 144.
//!
//! The tail after tp_flags (byte 168) follows CPython's order in both
//! layouts, so tp_methods (byte 232) and tp_getset (byte 248) are read from
//! the same offsets everywhere. Note that our *dynamic* stubs reserve byte
//! 248 for their real-type back pointer (`alloc_type_stub`), so table walks
//! are skipped for them.
//!
//! Callers must only dispatch on stubs whose slots hold genuine
//! `extern "C"` pointers: slot words copied verbatim out of Rust-internal vm
//! types (as `fill_type_stub` does today) hold Rust-ABI function bits that
//! must not be invoked through this module.

use core::ffi::{c_char, c_void};
use core::ptr::NonNull;

use rustpython_vm::builtins::PyStr;
use rustpython_vm::{AsObject, PyObject, PyObjectRef};

use crate::descrobject::PyGetSetDef;
use crate::methodobject::{PyMethodDef, build_method_def};
use crate::objectstatics::{StubKind, is_type_stub_addr};
use crate::pystate::with_vm;

/// `tp_call` slot: `PyObject *(*)(PyObject *self, PyObject *args, PyObject *kwds)`
type CCallFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;

/// `tp_getattro` slot: `PyObject *(*)(PyObject *self, PyObject *name)`
type CGetAttrFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

// Byte offsets of selected slots in a CPython-compatible PyTypeObject.
const STUB_CALL_BYTE: usize = 128;
const STUB_GETATTRO_BYTE: usize = 152; // our stub layout
const CPYTHON_GETATTRO_BYTE: usize = 144; // standard CPython layout
const STUB_TPNAME_BYTE: usize = 24;
const TAIL_METHODS_BYTE: usize = 232;
const TAIL_GETSET_BYTE: usize = 248;

// Upper bound for walking NUL-terminated descriptor tables, mirroring
// `method_def_count` in object::pytype.
const MAX_TABLE_ENTRIES: usize = 10_000;

/// Read the C-visible `ob_type` pointer at offset 8 of the object header,
/// where the type pointer lives for both foreign and native objects.
#[inline]
unsafe fn obj_type_ptr(obj: *const PyObject) -> *const usize {
    unsafe { *(obj as *const usize).add(1) as *const usize }
}

/// Read one pointer-sized slot word from a type object at a byte offset.
#[inline]
unsafe fn slot_at(ty: *const usize, byte_offset: usize) -> *mut c_void {
    let word = byte_offset / core::mem::size_of::<usize>();
    unsafe { *ty.add(word) as *mut c_void }
}

/// True when `addr` is one of this exe's exported type-stub symbols or the
/// relay's copies of them (see `objectstatics::is_type_stub_addr`).
fn is_exported_stub_addr(addr: usize) -> bool {
    use crate::objectstatics as statics;

    if is_type_stub_addr(addr, StubKind::Str)
        || is_type_stub_addr(addr, StubKind::Int)
        || is_type_stub_addr(addr, StubKind::Bool)
    {
        return true;
    }
    #[allow(static_mut_refs)]
    {
        fn stub_addresses() -> Vec<usize> {
            vec![
                core::ptr::addr_of!(statics::PyFloat_Type) as usize,
                core::ptr::addr_of!(statics::PySlice_Type) as usize,
                core::ptr::addr_of!(statics::PyType_Type) as usize,
                core::ptr::addr_of!(statics::PyBaseObject_Type) as usize,
                core::ptr::addr_of!(statics::PyBytes_Type) as usize,
                core::ptr::addr_of!(statics::PyCapsule_Type) as usize,
                core::ptr::addr_of!(statics::PyCFunction_Type) as usize,
                core::ptr::addr_of!(statics::PyComplex_Type) as usize,
                core::ptr::addr_of!(statics::PyDict_Type) as usize,
                core::ptr::addr_of!(statics::PyDictProxy_Type) as usize,
                core::ptr::addr_of!(statics::PyFrozenSet_Type) as usize,
                core::ptr::addr_of!(statics::PyGetSetDescr_Type) as usize,
                core::ptr::addr_of!(statics::PyList_Type) as usize,
                core::ptr::addr_of!(statics::PyMemberDescr_Type) as usize,
                core::ptr::addr_of!(statics::PyMemoryView_Type) as usize,
                core::ptr::addr_of!(statics::PyMethodDescr_Type) as usize,
                core::ptr::addr_of!(statics::PySet_Type) as usize,
                core::ptr::addr_of!(statics::PyTuple_Type) as usize,
            ]
        }
        stub_addresses().contains(&addr)
    }
}

/// True when `addr` is a known type stub address: one of the exported static
/// stubs, its relay copy, or a dynamically allocated stub from the cache in
/// `object::pytype`.
pub(crate) fn is_known_type_stub_addr(addr: usize) -> bool {
    if is_exported_stub_addr(addr) {
        return true;
    }
    crate::object::pytype::resolve_dynamic_stub_addr(addr).is_some()
}

/// Check whether a `PyObject` is a foreign object (a raw buffer allocated by
/// `_PyObject_New`): its type pointer at offset 8 must be a known type stub
/// address rather than a live RustPython type object.
///
/// Objects typed by an extension's own `PyTypeObject` (not one of our stubs)
/// are also foreign raw buffers but do not answer true here; the dispatch
/// functions below handle them regardless of this check.
pub unsafe fn is_foreign_object(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    let ty = unsafe { obj_type_ptr(obj) } as usize;
    ty != 0 && is_known_type_stub_addr(ty)
}

/// True when descriptor tables may be read from the type at `ty`. Dynamic
/// type stubs park their real-type back pointer at the tp_getset offset, so
/// they must never be walked as tables.
fn tables_allowed(ty: *const usize) -> bool {
    crate::object::pytype::resolve_dynamic_stub_addr(ty as usize).is_none()
}

/// Best-effort type name for error messages, read from tp_name (offset 24).
unsafe fn type_name(ty: *const usize) -> String {
    if ty.is_null() {
        return "foreign object".to_owned();
    }
    let name = unsafe { *ty.add(STUB_TPNAME_BYTE / 8) as *const c_char };
    if name.is_null() {
        return "foreign object".to_owned();
    }
    // Table/type names are static C strings owned by the extension.
    unsafe { core::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

/// Walk a NUL-terminated `PyGetSetDef` table looking for `want`.
unsafe fn find_getset_entry(
    ty: *const usize,
    want: &str,
) -> Option<&'static PyGetSetDef> {
    if ty.is_null() || !tables_allowed(ty) {
        return None;
    }
    let table = unsafe { slot_at(ty, TAIL_GETSET_BYTE) }.cast::<PyGetSetDef>();
    if table.is_null() {
        return None;
    }
    for i in 0..MAX_TABLE_ENTRIES {
        let entry = unsafe { &*table.add(i) };
        if entry.name.is_null() {
            return None;
        }
        if unsafe { c_name_eq(entry.name, want) } {
            return Some(entry);
        }
    }
    None
}

/// Walk a NUL-terminated `PyMethodDef` table looking for `want`.
unsafe fn find_method_entry(
    ty: *const usize,
    want: &str,
) -> Option<&'static PyMethodDef> {
    if ty.is_null() || !tables_allowed(ty) {
        return None;
    }
    let table = unsafe { slot_at(ty, TAIL_METHODS_BYTE) }.cast::<PyMethodDef>();
    if table.is_null() {
        return None;
    }
    for i in 0..MAX_TABLE_ENTRIES {
        let entry = unsafe { &*table.add(i) };
        if entry.ml_name.is_null() {
            return None;
        }
        if unsafe { c_name_eq(entry.ml_name, want) } {
            return Some(entry);
        }
    }
    None
}

/// Compare a NUL-terminated C string with a Rust string slice.
///
/// # Safety
/// `name` must point to a valid NUL-terminated string.
unsafe fn c_name_eq(name: *const c_char, want: &str) -> bool {
    unsafe { core::ffi::CStr::from_ptr(name).to_bytes() == want.as_bytes() }
}

/// Dispatch attribute access on a foreign object.
///
/// If the type's `tp_getattro` slot is set, the C function pointer is called
/// and its result returned unchanged (NULL meaning the callee raised).
/// Otherwise the attribute is resolved through the type's `tp_getset`
/// table (calling the entry's getter), then its `tp_methods` table (returning
/// a bound method); a missing name raises `AttributeError`.
pub unsafe fn foreign_getattr(obj: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    if obj.is_null() || name.is_null() {
        return with_vm(|vm| -> rustpython_vm::PyResult<*mut PyObject> {
            Err(vm.new_system_error("foreign getattr called with a NULL pointer"))
        });
    }
    let ty = unsafe { obj_type_ptr(obj) };
    // Our stubs shift tp_getattro one word later than standard headers.
    let getattro_byte = if !ty.is_null() && is_known_type_stub_addr(ty as usize) {
        STUB_GETATTRO_BYTE
    } else {
        CPYTHON_GETATTRO_BYTE
    };
    if !ty.is_null() {
        let tp_getattro = unsafe { slot_at(ty, getattro_byte) };
        if !tp_getattro.is_null() {
            let getattro: CGetAttrFunc = unsafe { core::mem::transmute(tp_getattro) };
            return unsafe { getattro(obj, name) };
        }
    }

    // tp_getattro unset: fall back to tp_getset / tp_methods lookups.
    with_vm(|vm| -> rustpython_vm::PyResult<*mut PyObject> {
        let name_obj = unsafe { &*name }.try_downcast_ref::<PyStr>(vm).map_err(|_| {
            vm.new_type_error("attribute name must be a string, not '".to_owned()
                + &unsafe { (&*name).class().to_string() }
                + "'")
        })?;
        let want = name_obj.to_str().unwrap_or("");

        if let Some(entry) = (unsafe { find_getset_entry(ty, want) }) {
            let Some(get) = entry.get else {
                return Err(vm.new_attribute_error(format!(
                    "attribute '{want}' of '{}' is not readable",
                    unsafe { type_name(ty) }
                )));
            };
            let ret = unsafe { get(obj, entry.closure) };
            return match NonNull::new(ret) {
                Some(ptr) => Ok(ptr.as_ptr()),
                None => Err(vm.take_raised_exception().unwrap_or_else(|| {
                    vm.new_system_error(
                        "foreign getter returned NULL, but there was no exception set",
                    )
                })),
            };
        }

        if let Some(md) = unsafe { find_method_entry(ty, want) } {
            let zelf: PyObjectRef = unsafe { (&*obj).to_owned() };
            let method: PyObjectRef =
                build_method_def(vm, md, true)?.build_function(vm, Some(zelf)).into();
            return Ok(method.into_raw().as_ptr());
        }

        Err(vm.new_attribute_error(format!(
            "'{}' object has no attribute '{want}'",
            unsafe { type_name(ty) }
        )))
    })
}

/// Dispatch calling a foreign object through its type's `tp_call` slot.
///
/// Returns NULL when the object has no type, the type has no `tp_call` slot,
/// or the C callee returned NULL after setting an exception; the caller is
/// responsible for surfacing the pending exception in the first two cases.
pub unsafe fn foreign_call(
    obj: *mut PyObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if obj.is_null() {
        return core::ptr::null_mut();
    }
    let ty = unsafe { obj_type_ptr(obj) };
    if ty.is_null() {
        return core::ptr::null_mut();
    }
    let tp_call = unsafe { slot_at(ty, STUB_CALL_BYTE) };
    if tp_call.is_null() {
        return core::ptr::null_mut();
    }
    let call: CCallFunc = unsafe { core::mem::transmute(tp_call) };
    unsafe { call(obj, args, kwds) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake type object big enough for the tail offsets.
    type FakeType = Box<[usize; 40]>;

    fn fake_type() -> FakeType {
        Box::new([0; 40])
    }

    /// A minimal raw foreign object header pointing at `ty`.
    ///
    /// Returns the boxed storage so it stays alive for the test.
    fn fake_object(ty: *const usize) -> (Box<[usize; 2]>, *mut PyObject) {
        let mut storage = Box::new([1usize, ty as usize]);
        let ptr = storage.as_mut_ptr() as *mut PyObject;
        (storage, ptr)
    }

    #[test]
    fn slot_word_offsets_match_layout() {
        let word = core::mem::size_of::<usize>();
        assert_eq!(STUB_CALL_BYTE / word, 16);
        assert_eq!(CPYTHON_GETATTRO_BYTE / word, 18);
        assert_eq!(STUB_GETATTRO_BYTE / word, 19);
        assert_eq!(TAIL_METHODS_BYTE / word, 29);
        assert_eq!(TAIL_GETSET_BYTE / word, 31);
        assert_eq!(STUB_TPNAME_BYTE / word, 3);
    }

    #[test]
    fn is_foreign_object_checks_ob_type_stub_addr() {
        unsafe {
            assert!(!is_foreign_object(core::ptr::null_mut()));

            // ob_type = NULL: not foreign.
            let (storage, obj) = fake_object(core::ptr::null());
            assert!(!is_foreign_object(obj));
            drop(storage);

            // ob_type = an unknown address (the fake type itself): not foreign.
            let mut ty = fake_type();
            let ty_ptr = ty.as_mut_ptr() as *const usize;
            let (storage, obj) = fake_object(ty_ptr);
            assert!(!is_foreign_object(obj));
            drop(storage);

            // ob_type = the exported PyUnicode_Type stub: foreign.
            #[allow(static_mut_refs)]
            let stub_addr = core::ptr::addr_of!(crate::objectstatics::PyUnicode_Type) as usize;
            let (storage, obj) = fake_object(stub_addr as *const usize);
            assert!(is_foreign_object(obj));
            drop(storage);
        }
    }

    #[test]
    fn foreign_call_dispatches_to_tp_call() {
        static mut LAST_SELF: usize = 0;
        static mut LAST_KWDS: usize = 0;
        const SENTINEL: *mut PyObject = 0x21 as *mut PyObject;

        unsafe extern "C" fn fake_call(
            slf: *mut PyObject,
            _args: *mut PyObject,
            kwds: *mut PyObject,
        ) -> *mut PyObject {
            unsafe {
                LAST_SELF = slf as usize;
                LAST_KWDS = kwds as usize;
            }
            SENTINEL
        }

        unsafe {
            let mut ty = fake_type();
            let ty_ptr = ty.as_mut_ptr() as *const usize;
            ty[STUB_CALL_BYTE / 8] = fake_call as usize;
            let (_obj_storage, obj) = fake_object(ty_ptr);
            let args = 0x11 as *mut PyObject;
            let kwds = 0x22 as *mut PyObject;

            assert_eq!(foreign_call(obj, args, kwds), SENTINEL);
            assert_eq!(LAST_SELF, obj as usize);
            assert_eq!(LAST_KWDS, kwds as usize);

            // No tp_call slot: NULL result.
            let mut empty_ty = fake_type();
            let (_, empty_obj) = fake_object(empty_ty.as_mut_ptr() as *const usize);
            assert!(foreign_call(empty_obj, args, kwds).is_null());

            // NULL self: NULL result.
            assert!(foreign_call(core::ptr::null_mut(), args, kwds).is_null());
        }
    }

    #[test]
    fn foreign_getattr_dispatches_to_tp_getattro() {
        const SENTINEL: *mut PyObject = 0x33 as *mut PyObject;

        unsafe extern "C" fn fake_getattro(
            _slf: *mut PyObject,
            _name: *mut PyObject,
        ) -> *mut PyObject {
            SENTINEL
        }

        unsafe {
            // Our stub layout: tp_getatto at byte 152.
            let mut stub_ty = fake_type();
            let stub_ty_ptr = stub_ty.as_mut_ptr() as *const usize;
            stub_ty[STUB_GETATTRO_BYTE / 8] = fake_getattro as usize;
            let (_, stub_obj) = fake_object(stub_ty_ptr);
            assert_eq!(
                foreign_getattr(stub_obj, 0x44 as *mut PyObject),
                SENTINEL
            );

            // Standard CPython layout: tp_getattro at byte 144.
            let mut ext_ty = fake_type();
            let ext_ty_ptr = ext_ty.as_mut_ptr() as *const usize;
            ext_ty[CPYTHON_GETATTRO_BYTE / 8] = fake_getattro as usize;
            let (_, ext_obj) = fake_object(ext_ty_ptr);
            assert_eq!(
                foreign_getattr(ext_obj, 0x44 as *mut PyObject),
                SENTINEL
            );
        }
    }

    #[test]
    fn foreign_getattr_tp_getset_and_methods_fallback() {
        use pyo3::prelude::*;
        use pyo3::types::PyString;

        const GETTER_SENTINEL: *mut PyObject = 0x55 as *mut PyObject;
        static mut GETTER_CLOSURE_SEEN: usize = 0;

        unsafe extern "C" fn fake_getter(
            slf: *mut PyObject,
            closure: *mut c_void,
        ) -> *mut PyObject {
            unsafe { GETTER_CLOSURE_SEEN = closure as usize };
            // Mark that the getter observed the foreign self pointer.
            assert!(!slf.is_null());
            GETTER_SENTINEL
        }

        unsafe extern "C" fn fake_noargs_method(
            _slf: *mut PyObject,
            _args: *mut PyObject,
        ) -> *mut PyObject {
            // METH_NOARGS: args is NULL; return a real int for the call check.
            unsafe { pyo3::ffi::PyLong_FromLong(4242) }.cast()
        }

        const METH_VARARGS: std::ffi::c_int = 0x0001;
        const METH_NOARGS: std::ffi::c_int = 0x0004;

        Python::attach(|py| {
            unsafe {
                let name = PyString::new(py, "answer");
                let missing = PyString::new(py, "no_such_attr");
                let method_name = PyString::new(py, "twice");

                // An extension-owned type struct: no tp_getattro, but real
                // tp_getset (byte 248) and tp_methods (byte 232) tables.
                let mut ty = fake_type();
                let ty_ptr = ty.as_mut_ptr() as *const usize;
                let getset = [
                    PyGetSetDef {
                        name: c"answer".as_ptr(),
                        get: Some(fake_getter),
                        set: None,
                        doc: core::ptr::null(),
                        closure: 0x7E57 as *mut c_void,
                    },
                    PyGetSetDef {
                        name: core::ptr::null(),
                        get: None,
                        set: None,
                        doc: core::ptr::null(),
                        closure: core::ptr::null_mut(),
                    },
                ];
                ty[TAIL_GETSET_BYTE / 8] = getset.as_ptr() as usize;
                let methods = [
                    PyMethodDef {
                        ml_name: c"twice".as_ptr(),
                        ml_meth: crate::methodobject::PyMethodPointer {
                            PyCFunction: Some(fake_noargs_method),
                        },
                        ml_flags: METH_NOARGS,
                        ml_doc: core::ptr::null(),
                    },
                    PyMethodDef {
                        ml_name: core::ptr::null(),
                        ml_meth: crate::methodobject::PyMethodPointer { PyCFunction: None },
                        ml_flags: METH_VARARGS,
                        ml_doc: core::ptr::null(),
                    },
                ];
                ty[TAIL_METHODS_BYTE / 8] = methods.as_ptr() as usize;

                let (_obj_storage, obj) = fake_object(ty_ptr);

                // tp_getset fallback: the getter runs with the closure.
                assert_eq!(
                    foreign_getattr(obj, name.as_ptr()),
                    GETTER_SENTINEL
                );
                assert_eq!(GETTER_CLOSURE_SEEN, 0x7E57);

                // tp_methods fallback: returns a bound callable.
                let bound = foreign_getattr(obj, method_name.as_ptr());
                assert!(!bound.is_null());
                let callable = py.from_owned_ptr(bound);
                let value = callable.call0().unwrap();
                assert_eq!(value.extract::<i64>().unwrap(), 4242);

                // Unknown attribute: NULL with an AttributeError pending.
                assert!(foreign_getattr(obj, missing.as_ptr()).is_null());
                let err = pyo3::PyErr::take(py).unwrap();
                assert!(err.is_instance_of::<pyo3::exceptions::PyAttributeError>(py));
                assert!(
                    err.to_string().contains("'foreign object' object has no attribute 'no_such_attr'")
                        || err.to_string().contains("no_such_attr")
                );

                // Non-string name: rejected without touching the tables.
                assert!(foreign_getattr(obj, pyo3::ffi::PyLong_FromLong(1)).is_null());
            }
        })
    }
}
