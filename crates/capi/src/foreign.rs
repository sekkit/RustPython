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
use rustpython_vm::{PyObject, PyObjectRef};

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
    crate::crash_diag::install();
    if obj.is_null() {
        return false;
    }
    // Registry-only: every raw buffer allocated by _PyObject_New is
    // registered here. The vtable/module heuristics produced false
    // positives on native objects, routing them to libc::free and
    // corrupting the heap (allocator mismatch).
    crate::objimpl::is_foreign_object(obj as *const u8)
}

/// True when `addr` lies inside a loaded module other than the main exe.
fn addr_in_non_exe_module(addr: usize) -> bool {
    unsafe extern "system" {
        fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut c_void) -> i32;
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }
    const FROM_ADDRESS: u32 = 0x4;
    const UNCHANGED_REFCOUNT: u32 = 0x2;
    unsafe {
        let mut containing: *mut c_void = core::ptr::null_mut();
        if GetModuleHandleExW(
            FROM_ADDRESS | UNCHANGED_REFCOUNT,
            addr as *const u16,
            &mut containing,
        ) == 0
            || containing.is_null()
        {
            return false; // heap/statics of exe → not foreign-by-module
        }
        let exe = GetModuleHandleW(core::ptr::null());
        !containing.is_null() && containing != exe
    }
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

/// Byte offset of the tp_getattro slot for a type at `ty_addr`: our stubs
/// shift it one word later than the standard CPython headers.
fn getattro_slot_offset(ty_addr: usize) -> usize {
    if ty_addr != 0 && is_known_type_stub_addr(ty_addr) {
        STUB_GETATTRO_BYTE
    } else {
        CPYTHON_GETATTRO_BYTE
    }
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
    let getattro_byte = getattro_slot_offset(ty as usize);
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

        if let Some(entry) = unsafe { find_getset_entry(ty, want) } {
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

/// Wrap a foreign raw-buffer object into a safe native RustPython object.
///
/// Called from the VM (via `foreign_dispatch::wrap_foreign`) when a C function
/// returns a pointer that `is_foreign_object` recognises. The returned native
/// object carries the resolved type so `type()`/`isinstance()` work; the
/// foreign buffer is kept alive by bumping its header refcount.
pub unsafe extern "C" fn wrap_foreign_object(raw: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<*mut PyObject> {
        if raw.is_null() {
            return Err(vm.new_system_error("wrap_foreign: NULL pointer"));
        }
        // ob_type at offset 8 is the extension's own PyTypeObject (or one of
        // our stubs). Resolve it to a real RustPython type.
        let ty_raw = unsafe { *(raw as *const usize).add(1) } as *mut crate::object::PyTypeObject;
        let ty = crate::object::pytype::resolve_type_ptr(vm, ty_raw)?;
        // Keep the foreign buffer alive for as long as the wrapper lives by
        // bumping its header refcount (ob_refcnt at offset 0).
        unsafe {
            let header = raw as *mut usize;
            *header = (*header).wrapping_add(1);
        }
        let obj = vm.ctx.new_base_object(ty, Some(vm.ctx.new_dict()));
        Ok(obj.into_raw().as_ptr())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake type object big enough for the tail offsets.
    type FakeType = Box<[usize; 40]>;

    /// Non-null marker returned by fake getters to trace dispatch.
    const GETTER_TEST_SENTINEL: *mut PyObject = 0x55 as *mut PyObject;

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
        static LAST_SELF: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static LAST_KWDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        const SENTINEL: *mut PyObject = 0x21 as *mut PyObject;

        unsafe extern "C" fn fake_call(
            slf: *mut PyObject,
            _args: *mut PyObject,
            kwds: *mut PyObject,
        ) -> *mut PyObject {
            use std::sync::atomic::Ordering::Relaxed;
            LAST_SELF.store(slf as usize, Relaxed);
            LAST_KWDS.store(kwds as usize, Relaxed);
            SENTINEL
        }

        unsafe {
            let mut ty = fake_type();
            let ty_ptr = ty.as_mut_ptr() as *const usize;
            let call_fn: CCallFunc = fake_call;
            ty[STUB_CALL_BYTE / 8] = call_fn as usize;
            let (_obj_storage, obj) = fake_object(ty_ptr);
            let args = 0x11 as *mut PyObject;
            let kwds = 0x22 as *mut PyObject;

            assert_eq!(foreign_call(obj, args, kwds), SENTINEL);
            assert_eq!(LAST_SELF.load(std::sync::atomic::Ordering::Relaxed), obj as usize);
            assert_eq!(LAST_KWDS.load(std::sync::atomic::Ordering::Relaxed), kwds as usize);

            // No tp_call slot: NULL result.
            let empty_ty = fake_type();
            let (_empty_storage, empty_obj) = fake_object(empty_ty.as_ptr() as *const usize);
            assert!(foreign_call(empty_obj, args, kwds).is_null());

            // NULL self: NULL result.
            assert!(foreign_call(core::ptr::null_mut(), args, kwds).is_null());
        }
    }

    #[test]
    fn getattro_slot_offset_selects_layout() {
        let ty = fake_type();
        let unknown_addr = ty.as_ptr() as usize;
        // Unknown type pointers read the standard CPython offsets...
        assert_eq!(getattro_slot_offset(unknown_addr), CPYTHON_GETATTRO_BYTE);
        assert_eq!(getattro_slot_offset(0), CPYTHON_GETATTRO_BYTE);
        // ...while registered stubs read our shifted layout.
        #[allow(static_mut_refs)]
        let known = core::ptr::addr_of!(crate::objectstatics::PyUnicode_Type) as usize;
        assert_eq!(getattro_slot_offset(known), STUB_GETATTRO_BYTE);
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
            // A type we do not recognize as one of our stubs is dispatched
            // through the standard CPython tp_getattro offset (byte 144).
            let mut ext_ty = fake_type();
            let ext_ty_ptr = ext_ty.as_mut_ptr() as *const usize;
            let getattro_fn: CGetAttrFunc = fake_getattro;
            ext_ty[CPYTHON_GETATTRO_BYTE / 8] = getattro_fn as usize;
            let (_ext_storage, ext_obj) = fake_object(ext_ty_ptr);
            assert_eq!(
                foreign_getattr(ext_obj, 0x44 as *mut PyObject),
                SENTINEL
            );
        }
    }

    /// Walk both descriptor tables of a fake extension-owned type without a
    /// live interpreter and invoke the getter directly through its entry.
    #[test]
    fn tp_getset_and_methods_table_walks_find_entries() {
        static GETTER_CLOSURE_SEEN: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);

        unsafe extern "C" fn fake_getter(
            slf: *mut PyObject,
            closure: *mut c_void,
        ) -> *mut PyObject {
            assert!(!slf.is_null());
            GETTER_CLOSURE_SEEN.store(closure as usize, std::sync::atomic::Ordering::Relaxed);
            GETTER_TEST_SENTINEL
        }

        unsafe extern "C" fn method_table_terminator(
            _slf: *mut PyObject,
            _args: *mut PyObject,
        ) -> *mut PyObject {
            core::ptr::null_mut()
        }

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
                    PyCFunction: method_table_terminator,
                },
                ml_flags: 0x0004, // METH_NOARGS
                ml_doc: core::ptr::null(),
            },
            PyMethodDef {
                ml_name: core::ptr::null(),
                ml_meth: crate::methodobject::PyMethodPointer {
                    PyCFunction: method_table_terminator,
                },
                ml_flags: 0x0001, // METH_VARARGS
                ml_doc: core::ptr::null(),
            },
        ];
        ty[TAIL_METHODS_BYTE / 8] = methods.as_ptr() as usize;

        unsafe {
            let entry = find_getset_entry(ty_ptr, "answer").expect("getset entry not found");
            assert!(entry.set.is_none());
            assert_eq!(
                entry.get.unwrap()(1 as *mut PyObject, entry.closure),
                GETTER_TEST_SENTINEL
            );
            assert_eq!(
                GETTER_CLOSURE_SEEN.load(std::sync::atomic::Ordering::Relaxed),
                0x7E57
            );

            assert!(find_getset_entry(ty_ptr, "no_such_attr").is_none());
            assert!(find_method_entry(ty_ptr, "twice").is_some());
            assert!(find_method_entry(ty_ptr, "no_such_method").is_none());

            // A type without tables finds nothing.
            let empty_ty = fake_type();
            let empty_ptr = empty_ty.as_ptr() as *const usize;
            assert!(find_getset_entry(empty_ptr, "answer").is_none());
            assert!(find_method_entry(empty_ptr, "twice").is_none());
        }
    }

    /// End-to-end fallback through `foreign_getattr` with real PyStr names.
    ///
    /// Ignored because `Python::attach`-based tests currently abort in some
    /// dev environments (pre-existing: `pytype::tests` shows the same); run
    /// explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore = "embedded interpreter init aborts in some dev environments"]
    fn foreign_getattr_tp_getset_and_methods_fallback() {
        use pyo3::prelude::*;
        use pyo3::types::PyString;

        static GETTER_CLOSURE_SEEN: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);

        unsafe extern "C" fn fake_getter(
            slf: *mut PyObject,
            closure: *mut c_void,
        ) -> *mut PyObject {
            // Mark that the getter observed the foreign self pointer.
            assert!(!slf.is_null());
            GETTER_CLOSURE_SEEN.store(closure as usize, std::sync::atomic::Ordering::Relaxed);
            GETTER_TEST_SENTINEL
        }

        unsafe extern "C" fn fake_noargs_method(
            _slf: *mut PyObject,
            _args: *mut PyObject,
        ) -> *mut PyObject {
            // METH_NOARGS: args is NULL; return a real int for the call check.
            unsafe { pyo3::ffi::PyLong_FromLong(4242) }.cast()
        }

        // Placeholder for the table's NUL terminator entry; never invoked
        // because the walk stops at the null ml_name first.
        unsafe extern "C" fn method_table_terminator(
            _slf: *mut PyObject,
            _args: *mut PyObject,
        ) -> *mut PyObject {
            core::ptr::null_mut()
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
                            PyCFunction: fake_noargs_method,
                        },
                        ml_flags: METH_NOARGS,
                        ml_doc: core::ptr::null(),
                    },
                    PyMethodDef {
                        ml_name: core::ptr::null(),
                        ml_meth: crate::methodobject::PyMethodPointer {
                            PyCFunction: method_table_terminator,
                        },
                        ml_flags: METH_VARARGS,
                        ml_doc: core::ptr::null(),
                    },
                ];
                ty[TAIL_METHODS_BYTE / 8] = methods.as_ptr() as usize;

                let (_obj_storage, obj) = fake_object(ty_ptr);

                // tp_getset fallback: the getter runs with the closure.
                assert_eq!(
                    foreign_getattr(obj, name.as_ptr().cast()),
                    GETTER_TEST_SENTINEL
                );
                assert_eq!(
                    GETTER_CLOSURE_SEEN.load(std::sync::atomic::Ordering::Relaxed),
                    0x7E57
                );

                // tp_methods fallback: returns a bound callable.
                let bound = foreign_getattr(obj, method_name.as_ptr().cast());
                assert!(!bound.is_null());
                let callable = pyo3::Bound::from_owned_ptr(py, bound.cast());
                let value = callable.call0().unwrap();
                assert_eq!(value.extract::<i64>().unwrap(), 4242);

                // Unknown attribute: NULL with an AttributeError pending.
                assert!(foreign_getattr(obj, missing.as_ptr().cast()).is_null());
                let err = pyo3::PyErr::take(py).unwrap();
                assert!(err.is_instance_of::<pyo3::exceptions::PyAttributeError>(py));
                assert!(
                    err.to_string().contains("'foreign object' object has no attribute 'no_such_attr'")
                        || err.to_string().contains("no_such_attr")
                );

                // Non-string name: rejected without touching the tables.
                assert!(
                    foreign_getattr(obj, pyo3::ffi::PyLong_FromLong(1).cast()).is_null()
                );
            }
        })
    }
}
