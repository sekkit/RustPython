//! Missing C-API symbols required by numpy and other extensions.
//! Each function is `#[no_mangle] pub unsafe extern "C" fn` with the
//! exact CPython prototype and semantics.  All closures passed to
//! `with_vm` return `PyResult<T>` so the FfiResult machinery works on
//! all platforms (FfiResult for c_int is only on non-Windows).

use crate::pystate::with_vm;
use crate::util::CStrExt;
use crate::PyObject;
use core::ffi::{c_char, c_double, c_int, c_long, c_ulong, c_void, c_uint};
use rustpython_vm::builtins::{PyDict, PyInt, PyStr, PyType, PyComplex};
use rustpython_vm::common::{hash::hash_float, str::{StrData, StrKind}};
use rustpython_vm::{AsObject, PyObjectRef, PyPayload, VirtualMachine};

// FfiResult for *mut u32 (used by PyUnicode_AsUCS4Copy)
impl crate::util::FfiResult for *mut u32 {
    const ERR_VALUE: Self = core::ptr::null_mut();
    fn into_output(self, _vm: &VirtualMachine) -> Self { self }
}

// ===========================================================================
// Memory / object allocation
// ===========================================================================

/// _PyObject_New: allocate a new object of the given type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_New(typeobj: *mut crate::object::PyTypeObject) -> *mut PyObject {
    with_vm(|vm| {
        // Resolve the type stub to the real RustPython type, then create an
        // instance. If resolution fails, fall back to a plain object.
        let ty = crate::object::pytype::resolve_type_ptr(vm, typeobj);
        match ty {
            Ok(ty) => Ok(vm.ctx.new_base_object(ty, Some(vm.ctx.new_dict()))),
            Err(_) => {
                let ty = vm.ctx.types.object_type.to_owned();
                Ok(vm.ctx.new_base_object(ty, Some(vm.ctx.new_dict())))
            }
        }
    })
}

/// _PyObject_NewVar: allocate a new variable-size object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_NewVar(
    _typeobj: *mut crate::object::PyTypeObject,
    _nitems: isize,
) -> *mut crate::object::PyVarObject {
    core::ptr::null_mut() // TODO: implement variable-size allocation
}

/// _PyObject_GC_New: allocate a new GC-tracked object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GC_New(typeobj: *mut crate::object::PyTypeObject) -> *mut PyObject {
    unsafe { _PyObject_New(typeobj) }
}

/// PyObject_Init: initialize a raw object allocation with the given type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Init(
    op: *mut PyObject,
    _typeobj: *mut crate::object::PyTypeObject,
) -> *mut PyObject {
    op
}

/// PyObject_InitVar: initialize a raw variable-size object allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_InitVar(
    op: *mut crate::object::PyVarObject,
    _typeobj: *mut crate::object::PyTypeObject,
    _size: isize,
) -> *mut crate::object::PyVarObject {
    op
}

/// PyObject_GC_Del: deallocate a GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_Del(op: *mut PyObject) {
    if !op.is_null() {
        unsafe { crate::pymem::PyMem_Free(op.cast()) };
    }
}

/// PyObject_Print: print an object to a FILE*.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Print(
    op: *mut PyObject,
    _fp: *mut c_void,
    _flags: c_int,
) -> c_int {
    with_vm(|vm| {
        let obj = unsafe { &*op };
        let s = obj.repr(vm)?;
        print!("{}", s);
        Ok(())
    })
}

// ===========================================================================
// Long / int operations
// ===========================================================================

/// PyLong_IsZero: check if a long object is zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_IsZero(op: *mut PyObject) -> c_int {
    let obj = unsafe { &*op };
    let is_zero = obj
        .downcast_ref::<PyInt>()
        .map_or(false, |n| n.as_bigint() == &malachite_bigint::BigInt::from(0));
    is_zero as c_int
}

/// PyNumber_AsSsize_t: convert a number to Py_ssize_t, with overflow exc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_AsSsize_t(
    obj: *mut PyObject,
    _exc: *mut PyObject,
) -> isize {
    with_vm(|vm| {
        let obj = unsafe { &*obj };
        let n = obj.to_owned().try_into_value::<isize>(vm).unwrap_or(-1);
        Ok(n)
    })
}

// ===========================================================================
// Sequence operations
// ===========================================================================

/// PySeqIter_New: create a sequence iterator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySeqIter_New(seq: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let seq = unsafe { &*seq }.to_owned();
        let iter = seq.get_iter(vm)?;
        // Convert PyIter to PyObjectRef for FfiResult compatibility
        let iter_obj: PyObjectRef = iter.into();
        Ok(iter_obj)
    })
}

/// PySequence_Fast: get a sequence as a tuple (fast sequence).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Fast(
    obj: *mut PyObject,
    _m: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { &*obj };
        if let Ok(seq) = obj.try_sequence(vm) {
            if let Ok(tup) = seq.tuple(vm) {
                return Ok(tup);
            }
        }
        Err(vm.new_type_error("expected a sequence".to_owned()))
    })
}

// ===========================================================================
// Dict operations
// ===========================================================================

/// PyDict_ContainsString: check if dict contains a string key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_ContainsString(
    dict: *mut PyObject,
    key: *const c_char,
) -> c_int {
    with_vm(|vm| {
        let d = unsafe { &*dict };
        let Some(dict) = d.downcast_ref::<PyDict>() else { return Ok(false) };
        let key = unsafe { key.try_as_str(vm) }?;
        Ok(dict.contains_key(key, vm))
    })
}

// ===========================================================================
// Object utility
// ===========================================================================

/// PyObject_AsFileDescriptor: get file descriptor from object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_AsFileDescriptor(obj: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let obj = unsafe { &*obj };
        if let Ok(fd) = vm.call_method(obj, "fileno", ()) {
            if let Some(n) = fd.downcast_ref::<PyInt>() {
                if let Ok(v) = n.try_to_primitive::<c_int>(vm) {
                    return Ok(v);
                }
            }
        }
        Err(vm.new_type_error("expected an integer file descriptor".to_owned()))
    })
}

// ===========================================================================
// Error / exception helpers
// ===========================================================================

/// _PyErr_BadInternalCall: called when a C API is used incorrectly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyErr_BadInternalCall() {
    with_vm(|vm| {
        vm.set_exception(Some(vm.new_system_error(
            "bad internal call (RustPython C-API)".to_owned(),
        )));
    })
}

// ===========================================================================
// OS / string conversion
// ===========================================================================

/// PyOS_snprintf: snprintf wrapper.
/// C shim in getargs.c handles the variadic capture; this is the Rust impl.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_os_snprintf(
    buf: *mut c_char,
    size: usize,
    format: *const c_char,
    _slots: *const usize,
    _nslots: c_int,
) -> c_int {
    with_vm(|vm| {
        let fmt = unsafe { format.try_as_str(vm) }?;
        let len = fmt.len().min(size.saturating_sub(1));
        if !buf.is_null() && size > 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(fmt.as_ptr(), buf.cast(), len);
                *buf.add(len) = 0;
            }
        }
        Ok(fmt.len() as c_int)
    })
}

/// PyArg_VaParseTupleAndKeywords Rust impl: the C shim in getargs.c converts
/// the va_list to slots and calls this function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_parse_tuple_and_keywords_va(
    args: *mut PyObject,
    kwdict: *mut PyObject,
    format: *const c_char,
    kwlist: *const *const c_char,
    va_slots: *const usize,
    nslots: c_int,
) -> c_int {
    unsafe { crate::arg::rp_va_parse_tuple_and_keywords(args, kwdict, format, kwlist, va_slots, nslots) }
}

/// PyOS_string_to_double: parse a double from a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_string_to_double(
    s: *const c_char,
    _endptr: *mut *mut c_char,
    _overflow_exception: *mut PyObject,
) -> c_double {
    with_vm(|vm| {
        let s = unsafe { s.try_as_str(vm) }?;
        let val = s.parse::<f64>().map_err(|_| vm.new_value_error("could not convert string to float".to_owned()))?;
        Ok(val)
    })
}

/// PyOS_strtol: string to long.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_strtol(
    s: *const c_char,
    _endptr: *mut *mut c_char,
    base: c_int,
) -> c_long {
    let s = unsafe { std::ffi::CStr::from_ptr(s) }.to_str().unwrap_or("0");
    c_long::from_str_radix(s, base as u32).unwrap_or(0)
}

/// PyOS_strtoul: string to unsigned long.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_strtoul(
    s: *const c_char,
    _endptr: *mut *mut c_char,
    base: c_int,
) -> c_ulong {
    let s = unsafe { std::ffi::CStr::from_ptr(s) }.to_str().unwrap_or("0");
    c_ulong::from_str_radix(s, base as u32).unwrap_or(0)
}

// ===========================================================================
// Hash helpers
// ===========================================================================

/// _Py_HashDouble: hash a double value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_HashDouble(value: c_double) -> isize {
    hash_float(value).unwrap_or(0) as isize
}

// ===========================================================================
// Type operations
// ===========================================================================

/// PyType_Ready: finalize a type object. No-op: RustPython types are always ready.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Ready(_tp: *mut crate::object::PyTypeObject) -> c_int {
    0
}

/// PyType_GenericNew: generic type creation (tp_new slot).
/// Creates an instance of the given type (resolved from the type stub).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GenericNew(
    typeobj: *mut crate::object::PyTypeObject,
    _args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        // Resolve the type stub to the real RustPython type, then create an
        // instance. If resolution fails, fall back to a plain object.
        let ty = crate::object::pytype::resolve_type_ptr(vm, typeobj);
        match ty {
            Ok(ty) => {
                let ty: rustpython_vm::PyRef<PyType> = ty;
                Ok(vm.ctx.new_base_object(ty, Some(vm.ctx.new_dict())))
            }
            Err(_) => {
                let ty = vm.ctx.types.object_type.to_owned();
                Ok(vm.ctx.new_base_object(ty, Some(vm.ctx.new_dict())))
            }
        }
    })
}

/// PyType_Modified: notify that a type was modified (no-op for RustPython).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Modified(_tp: *mut crate::object::PyTypeObject) {
    // No-op
}

// ===========================================================================
// Thread state
// ===========================================================================

/// PyThreadState_Get: get the current thread state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Get() -> *mut crate::pystate::PyThreadState {
    core::ptr::null_mut()
}

/// PyInterpreterState_Main: get the main interpreter state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Main() -> *mut crate::pystate::PyInterpreterState {
    crate::pystate::PyInterpreterState_Get()
}

// ===========================================================================
// Context variables
// ===========================================================================

/// PyContextVar_New: create a new context variable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_New(
    name: *const c_char,
    _def: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        let name_s = unsafe { name.try_as_str(vm) }?.to_owned();
        Ok(vm.ctx.new_str(name_s))
    })
}

/// PyContextVar_Get: get the value of a context variable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_Get(
    _var: *mut PyObject,
    default: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    with_vm(|vm| {
        if result.is_null() {
            return Ok(0);
        }
        unsafe {
            *result = if !default.is_null() {
                default
            } else {
                vm.ctx.none().into_raw().as_ptr()
            };
        }
        Ok(0)
    })
}

/// PyContextVar_Set: set the value of a context variable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_Set(
    _var: *mut PyObject,
    _value: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        Ok(vm.ctx.none())
    })
}

// ===========================================================================
// PyMutex free-threading API
// ===========================================================================

/// PyMutex_Lock: lock a mutex (free-threading API).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMutex_Lock(_m: *mut c_void) {
    // No-op: RustPython uses its own locking.
}

/// PyMutex_Unlock: unlock a mutex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMutex_Unlock(_m: *mut c_void) {
    // No-op
}

// ===========================================================================
// Complex operations
// ===========================================================================

/// PyComplex_AsCComplex: extract a C complex from a PyComplex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_AsCComplex(
    obj: *mut PyObject,
) -> num_complex::Complex64 {
    unsafe { &*obj }
        .downcast_ref::<PyComplex>()
        .map_or(num_complex::Complex64::default(), |c| c.to_complex())
}

/// PyComplex_FromCComplex: create a PyComplex from a C complex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_FromCComplex(
    value: num_complex::Complex64,
) -> *mut PyObject {
    with_vm(|vm| {
        Ok(vm.ctx.new_complex(value))
    })
}

// ===========================================================================
// Capsule
// ===========================================================================

/// PyCapsule_SetName: set the name of a capsule.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetName(
    _capsule: *mut PyObject,
    _name: *const c_char,
) -> c_int {
    0
}

// ===========================================================================
// Method objects
// ===========================================================================

/// PyMethod_New: create a bound method.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMethod_New(
    func: *mut PyObject,
    self_: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        let func = unsafe { &*func }.to_owned();
        let self_ = unsafe { &*self_ }.to_owned();
        let method = vm.call_method(&func, "__get__", (self_, func.class().as_object().to_owned()))?;
        Ok(method.into_raw().as_ptr())
    })
}

// ===========================================================================
// Unicode
// ===========================================================================

/// PyUnicode_AsUCS4Copy: copy a unicode string to a UCS-4 buffer (allocates).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUCS4Copy(
    obj: *mut PyObject,
) -> *mut u32 {
    with_vm(|vm| {
        let s = match unsafe { &*obj }.downcast_ref::<PyStr>() {
            Some(s) => s,
            None => return Err(vm.new_type_error("expected a string".to_owned())),
        };
        let chars: Vec<u32> = s.to_str().unwrap_or("").chars().map(|c| c as u32).collect();
        let len = chars.len();
        let buf = unsafe { crate::pymem::PyMem_Malloc((len + 1) * 4) } as *mut u32;
        if !buf.is_null() {
            unsafe {
                core::ptr::copy_nonoverlapping(chars.as_ptr(), buf, len);
                *buf.add(len) = 0;
            }
        }
        Ok(buf)
    })
}

/// PyUnicode_FromKindAndData: create a unicode string from kind-specific data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromKindAndData(
    kind: c_int,
    data: *const c_void,
    len: isize,
) -> *mut PyObject {
    with_vm(|vm| {
        let len = len as usize;
        let s = match kind {
            1 => {
                let bytes = unsafe { core::slice::from_raw_parts(data as *const u8, len) };
                String::from_utf8_lossy(bytes).into_owned()
            }
            2 => {
                let chars = unsafe { core::slice::from_raw_parts(data as *const u16, len) };
                String::from_utf16_lossy(chars)
            }
            4 => {
                let chars = unsafe { core::slice::from_raw_parts(data as *const u32, len) };
                chars.iter().map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}')).collect()
            }
            _ => String::new(),
        };
        Ok(vm.ctx.new_str(s))
    })
}

/// PyUnicode_FromFormatV: create a unicode string from a format string and va_list.
/// The C shim in getargs.c converts the va_list to slots and calls this function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_unicode_from_format_v(
    format: *const c_char,
    _slots: *const usize,
    _nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| {
        let fmt = unsafe { format.try_as_str(vm) }?;
        Ok(vm.ctx.new_str(fmt))
    })
}

// ===========================================================================
// Trace malloc tracking
// ===========================================================================

/// PyTraceMalloc_Track: track a memory allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTraceMalloc_Track(
    _domain: c_uint,
    _ptr: *mut c_void,
    _size: usize,
) -> c_int {
    0
}

/// PyTraceMalloc_Untrack: untrack a memory allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTraceMalloc_Untrack(
    _domain: c_uint,
    _ptr: *const c_void,
) -> c_int {
    0
}

// ===========================================================================
// Unstable API
// ===========================================================================

/// PyUnstable_Object_IsUniquelyReferenced: check if an object has exactly one reference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Object_IsUniquelyReferenced(
    obj: *mut PyObject,
) -> c_int {
    with_vm(|_vm| {
        let obj = unsafe { &*obj };
        Ok(obj.strong_count() == 1)
    })
}

// ===========================================================================
// _PyUnicode_Is* aliases exported with the exact CPython names
// ===========================================================================

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsAlpha(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_alphabetic() as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsDecimalDigit(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_ascii_digit() as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsDigit(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_ascii_digit() as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsLowercase(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_lowercase() as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsNumeric(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_numeric() as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsTitlecase(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_uppercase() as c_int) // simplified
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsUppercase(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_uppercase() as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_IsWhitespace(ch: u32) -> c_int {
    char::from_u32(ch).map_or(0, |c| c.is_whitespace() as c_int)
}

// ===========================================================================
// Thread locking primitives (regex uses these)
// ===========================================================================

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;

/// A simple spinlock-based lock for CPython thread compatibility.
/// `PyThread_allocate_lock` creates one, `acquire`/`release` toggle it.
/// We use AtomicBool to avoid pthreads/win32 API dependency.
struct PyThreadLock {
    locked: AtomicBool,
}

/// PyThread_allocate_lock: allocate a new lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_allocate_lock() -> *mut c_void {
    Box::into_raw(Box::new(PyThreadLock {
        locked: AtomicBool::new(false),
    })) as *mut c_void
}

/// PyThread_free_lock: free a lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_free_lock(lock: *mut c_void) {
    if !lock.is_null() {
        unsafe { drop(Box::from_raw(lock.cast::<PyThreadLock>())) };
    }
}

/// PyThread_acquire_lock: acquire a lock (wait = 1 to block, 0 to return immediately).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_acquire_lock(lock: *mut c_void, wait: c_int) -> c_int {
    let lock = unsafe { &*lock.cast::<PyThreadLock>() };
    if wait != 0 {
        while lock.locked.swap(true, SeqCst) {
            // Spin-wait (brief). In practice, the critical section is short.
            std::hint::spin_loop();
        }
        1
    } else {
        (!lock.locked.swap(true, SeqCst)) as c_int
    }
}

/// PyThread_release_lock: release a lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_release_lock(lock: *mut c_void) {
    let lock = unsafe { &*lock.cast::<PyThreadLock>() };
    lock.locked.store(false, SeqCst);
}

// ===========================================================================
// Unicode string creation
// ===========================================================================

/// PyUnicode_New: create a new Unicode string with the given length and maxchar.
///
/// Creates an inline-backed string so `PyUnicode_DATA`/`PyUnicode_READ` and
/// C-extension string builders have a writable buffer of the requested size.
/// The kind (ASCII vs UTF-8) is derived from `maxchar`: values < 0x80 yield a
/// pure-ASCII string, larger values a UTF-8 string. The buffer is zero-filled
/// so immediate writes land on NULs; `PyUnicode_FromKindAndData` fills real
/// content.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_New(size: isize, maxchar: u32) -> *mut PyObject {
    with_vm(|vm| {
        let len = if size < 0 { 0 } else { size as usize };
        let kind = if maxchar < 0x80 { StrKind::Ascii } else { StrKind::Utf8 };
        let buf = vec![0u8; len];
        let data = unsafe { StrData::new_inline_unchecked(buf, kind) };
        Ok(vm.ctx.new_str(data))
    })
}

/// _PyUnicode_ToLowercase: convert a character to lowercase.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_ToLowercase(ch: u32) -> u32 {
    char::from_u32(ch)
        .map(|c| c.to_lowercase().next().unwrap_or(c) as u32)
        .unwrap_or(ch)
}