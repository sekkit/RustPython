//! Slot dispatch for foreign C extension types.
//!
//! Foreign C extension types carry `tp_*` function pointers in their
//! CPython-compatible `PyTypeObject` stub (see `object::pytype` for the
//! writer side of that layout). These helpers read a slot out of the stub
//! and invoke it, so calls and attribute access on foreign objects can be
//! routed through the C functions the extension provided.

use core::ffi::c_void;
use rustpython_vm::PyObject;

use crate::object::PyObject_GenericGetAttr;

/// `tp_call` slot: `PyObject *(*)(PyObject *self, PyObject *args, PyObject *kwds)`
type CCallFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;

/// `tp_getattro` slot: `PyObject *(*)(PyObject *self, PyObject *name)`
type CGetAttrFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

/// Stub byte offsets of `tp_call` and `tp_getattro`, as `usize` word indexes
/// (`tp_call` at byte 128, `tp_getattro` at byte 152).
const STUB_CALL_WORD: usize = 128 / core::mem::size_of::<usize>();
const STUB_GETATTRO_WORD: usize = 152 / core::mem::size_of::<usize>();

/// Read the C-visible `ob_type` pointer at offset 8 of the object header,
/// where the type stub pointer lives for both foreign and native objects.
#[inline]
unsafe fn obj_type_ptr(obj: *const PyObject) -> *const usize {
    unsafe { *(obj as *const usize).add(1) as *const usize }
}

/// Read one slot word from a type stub.
#[inline]
unsafe fn stub_slot(ty: *const usize, word: usize) -> *mut c_void {
    unsafe { *(ty.add(word)) as *mut c_void }
}

/// Call a foreign object through its type's `tp_call` slot. Returns NULL when
/// the slot is not set; the caller is responsible for raising an error.
pub unsafe fn foreign_call(
    obj: *mut PyObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    let ty = unsafe { obj_type_ptr(obj) };
    if ty.is_null() {
        return core::ptr::null_mut();
    }
    let tp_call = unsafe { stub_slot(ty, STUB_CALL_WORD) };
    if tp_call.is_null() {
        return core::ptr::null_mut();
    }
    let call: CCallFunc = unsafe { core::mem::transmute(tp_call) };
    unsafe { call(obj, args, kwds) }
}

/// Get an attribute of a foreign object through its type's `tp_getattro`
/// slot. Falls back to `PyObject_GenericGetAttr` when the slot is not set.
pub unsafe fn foreign_getattr(obj: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let ty = unsafe { obj_type_ptr(obj) };
    let tp_getattro = if ty.is_null() {
        core::ptr::null_mut()
    } else {
        unsafe { stub_slot(ty, STUB_GETATTRO_WORD) }
    };
    if tp_getattro.is_null() {
        unsafe { PyObject_GenericGetAttr(obj, name) }
    } else {
        let getattro: CGetAttrFunc = unsafe { core::mem::transmute(tp_getattro) };
        unsafe { getattro(obj, name) }
    }
}
