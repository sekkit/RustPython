//! `PyArg_*` argument parsing, `Py_BuildValue` and the `PyObject_Call*`
//! variadic helpers (CPython Python/getargs.c, Objects/modsupport.c and
//! Objects/abstract.c equivalents).
//!
//! The C-variadic ABI is provided by the small C shim in shim/getargs.c,
//! which snapshots the variadic arguments as uniform `usize` slots; all
//! parsing and conversion logic lives here.

use crate::PyObject;
use crate::buffer::{PyBUF_FORMAT, PyBUF_WRITABLE, Py_buffer};
use crate::object::pytype::PyTypeObject;
use crate::pystate::with_vm;
use crate::util::CStrExt;
use core::ffi::{CStr, c_char, c_double, c_float, c_int, c_short, c_void};
use num_complex::Complex64;
use rustpython_vm::builtins::{
    try_bigint_to_f64, PyByteArray, PyBytes, PyComplex, PyDict, PyFloat, PyInt, PyList, PyNone,
    PyStr, PyTuple, PyType,
};
use rustpython_vm::function::FuncArgs;
use rustpython_vm::vm::thread::with_current_vm;
use rustpython_vm::{AsObject, PyObjectRef, PyResult, VirtualMachine};

/// NUL-terminated UTF-8 cache for the `s`/`z` format codes: CPython keeps a
/// NUL-terminated UTF-8 representation inside every str object; RustPython
/// does not, so we memoize one per object. The strong reference pins the
/// object (a deliberate, bounded leak), keeping the pointer valid.
type CStrCache = std::sync::Mutex<std::collections::HashMap<usize, (PyObjectRef, alloc::ffi::CString)>>;
static C_STR_CACHE: std::sync::LazyLock<CStrCache> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn cached_cstr(vm: &VirtualMachine, obj: &PyObjectRef, data: &[u8]) -> PyResult<*const c_char> {
    let key = obj.as_object().as_raw() as usize;
    let mut cache = C_STR_CACHE.lock().expect("C_STR_CACHE poisoned");
    if let Some((_, c)) = cache.get(&key) {
        return Ok(c.as_ptr());
    }
    let c = alloc::ffi::CString::new(data)
        .map_err(|_| vm.new_value_error("embedded null byte in argument string"))?;
    let ptr = c.as_ptr();
    cache.insert(key, (obj.clone(), c));
    Ok(ptr)
}

/// NUL-terminated UTF-16 cache for the `u`/`u#` format codes (wchar_t API).
type Utf16Cache = std::sync::Mutex<std::collections::HashMap<usize, (PyObjectRef, Vec<u16>)>>;
static UTF16_CACHE: std::sync::LazyLock<Utf16Cache> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn cached_utf16(vm: &VirtualMachine, s: &PyStrRef) -> PyResult<*const libc::wchar_t> {
    let text = s
        .to_str()
        .ok_or_else(|| vm.new_system_error("string contains surrogates"))?;
    let key = s.as_object().as_raw() as usize;
    let mut cache = UTF16_CACHE.lock().expect("UTF16_CACHE poisoned");
    cache.entry(key).or_insert_with(|| (s.clone().into(), text.encode_utf16().collect()));
    Ok(cache.get(&key).unwrap().1.as_ptr().cast())
}

type PyStrRef = rustpython_vm::PyRef<PyStr>;

/// The snapshot of variadic slots handed over from the C shim.
pub(crate) struct VaSlots<'a> {
    slots: &'a [usize],
    pos: usize,
}

impl<'a> VaSlots<'a> {
    pub(crate) fn new(slots: &'a [usize]) -> Self {
        Self { slots, pos: 0 }
    }

    fn take(&mut self, vm: &VirtualMachine) -> PyResult<usize> {
        if self.pos >= self.slots.len() {
            return Err(vm.new_system_error(
                "not enough varargs passed to the C shim (format/varargs mismatch)",
            ));
        }
        let v = self.slots[self.pos];
        self.pos += 1;
        Ok(v)
    }
}

/// Position cursor over the argument items, with support for the `()`/`[]`
/// nesting of the format language.
struct Cursor {
    levels: Vec<(Vec<PyObjectRef>, usize)>,
}

impl Cursor {
    fn new(items: Vec<PyObjectRef>) -> Self {
        Self {
            levels: vec![(items, 0)],
        }
    }

    fn cur_opt(&self) -> Option<PyObjectRef> {
        let (items, idx) = self.levels.last().expect("cursor has no levels");
        items.get(*idx).cloned()
    }

    fn advance(&mut self) {
        self.levels.last_mut().expect("cursor has no levels").1 += 1;
    }

    fn push(&mut self, items: Vec<PyObjectRef>) {
        self.levels.push((items, 0));
    }

    fn pop(&mut self) {
        self.levels.pop();
    }

    fn top_len(&self) -> usize {
        self.levels[0].0.len()
    }

    fn top_pos(&self) -> usize {
        self.levels[0].1
    }
}

fn type_name_of(item: &PyObjectRef) -> String {
    item.class().name().to_owned()
}

fn arg_type_error<T>(
    vm: &VirtualMachine,
    argno: usize,
    expected: &str,
    item: &PyObjectRef,
) -> PyResult<T> {
    Err(vm.new_type_error(format!(
        "argument {argno} must be {expected}, not {}",
        type_name_of(item)
    )))
}

/// How many extra format bytes a multi-character conversion code uses.
fn extra_code_bytes(rest: &[u8]) -> usize {
    match rest[0] {
        b'O' if matches!(rest.get(1), Some(b'!' | b'&')) => 1,
        b'e' if matches!(rest.get(1), Some(b's' | b't')) => {
            1 + usize::from(rest.get(2) == Some(&b'#'))
        }
        b's' | b'z' | b'y' | b'u' | b'w' | b't' if rest.get(1) == Some(&b'#') => 1,
        _ => 0,
    }
}

/// Zero an output slot of the given code: writes 0 with the width the code's
/// output type uses (NULL pointers, 0 integers, 0.0 doubles). `kind` is
/// "out" for output slots, "len" for the length half of `x#` codes, and
/// "skip" for input slots (O& converter, O! type, es/et encoding).
fn zero_slot(code: u8, kind: &str, slot: usize) {
    if slot == 0 {
        return;
    }
    let width: usize = match (code, kind) {
        (_, "skip") => return,
        (b'D', _) => 2 * core::mem::size_of::<c_double>(),
        (b'b' | b'B' | b'c', _) => 1,
        (b'h' | b'H', _) => 2,
        (b'f', _) => core::mem::size_of::<c_float>(),
        (b'i' | b'I' | b'C' | b'p', _) => core::mem::size_of::<c_int>(),
        (b'l', _) => core::mem::size_of::<core::ffi::c_long>(),
        (b'k', _) => core::mem::size_of::<core::ffi::c_ulong>(),
        (b'L' | b'K' | b'n' | b'd', _) => 8,
        (b'w', "star") => core::mem::size_of::<Py_buffer>(),
        (b's' | b'z' | b'y' | b'u', "star") => core::mem::size_of::<Py_buffer>(),
        (_, "len") => core::mem::size_of::<isize>(),
        _ => core::mem::size_of::<usize>(),
    };
    unsafe { core::ptr::write_bytes(slot as *mut u8, 0, width) };
}

/// Write the defaults for an absent optional argument: every output slot is
/// zeroed, input slots are skipped.
fn write_defaults(vm: &VirtualMachine, code: u8, rest: &[u8], slots: &mut VaSlots<'_>) -> PyResult<()> {
    match code {
        b'O' if rest.get(1) == Some(&b'!') => {
            let _ty = slots.take(vm)?;
            zero_slot(b'O', "out", slots.take(vm)?);
            Ok(())
        }
        b'O' if rest.get(1) == Some(&b'&') => {
            let _conv = slots.take(vm)?;
            zero_slot(b'O', "out", slots.take(vm)?);
            Ok(())
        }
        b'e' if matches!(rest.get(1), Some(b's' | b't')) => {
            let _enc = slots.take(vm)?;
            zero_slot(code, "out", slots.take(vm)?);
            if rest.get(2) == Some(&b'#') {
                zero_slot(code, "len", slots.take(vm)?);
            }
            Ok(())
        }
        b's' | b'z' | b'y' | b'u' | b't' | b'w' if rest.get(1) == Some(&b'#') => {
            zero_slot(code, "out", slots.take(vm)?);
            zero_slot(code, "len", slots.take(vm)?);
            Ok(())
        }
        b's' | b'z' | b'y' | b'w' if rest.get(1) == Some(&b'*') => {
            zero_slot(code, "star", slots.take(vm)?);
            Ok(())
        }
        _ => {
            zero_slot(code, "out", slots.take(vm)?);
            Ok(())
        }
    }
}

/// Convert one parse-side format unit. `rest` starts at `code`; returns the
/// number of format bytes consumed.
fn convert_one(
    vm: &VirtualMachine,
    code: u8,
    rest: &[u8],
    item: &PyObjectRef,
    slots: &mut VaSlots<'_>,
    argno: usize,
) -> PyResult<usize> {
    match code {
        b's' | b'z' | b'y' if rest.get(1) == Some(&b'#') => {
            let out: *mut *const c_char = slots.take(vm)? as *mut _;
            let out_len: *mut isize = slots.take(vm)? as *mut _;
            let allow_none = code == b'z';
            if allow_none && item.downcast_ref::<PyNone>().is_some() {
                unsafe {
                    *out = core::ptr::null();
                    *out_len = 0;
                }
                return Ok(2);
            }
            let data: &[u8] = if let Some(s) = item.downcast_ref::<PyStr>() {
                let s = s.try_as_utf8(vm)?;
                s.as_str().as_bytes()
            } else if let Some(b) = item.downcast_ref::<PyBytes>() {
                b.as_bytes()
            } else if let Some(ba) = item.downcast_ref::<PyByteArray>() {
                let data = ba.borrow_buf();
                let slice = data.to_vec();
                Box::leak(slice.into_boxed_slice())
            } else {
                let expected = if allow_none {
                    "str, bytes, bytearray or None"
                } else {
                    "str, bytes or bytearray"
                };
                return arg_type_error(vm, argno, expected, item);
            };
            unsafe {
                *out = data.as_ptr().cast();
                *out_len = data.len() as isize;
            }
            Ok(2)
        }
        b's' | b'z' if !matches!(rest.get(1), Some(b'#' | b'*')) => {
            let out: *mut *const c_char = slots.take(vm)? as *mut _;
            if code == b'z' && item.downcast_ref::<PyNone>().is_some() {
                unsafe { *out = core::ptr::null() };
                return Ok(1);
            }
            let ptr = if let Some(s) = item.downcast_ref::<PyStr>() {
                let s = s.try_as_utf8(vm)?;
                cached_cstr(vm, item, s.as_str().as_bytes())?
            } else if let Some(b) = item.downcast_ref::<PyBytes>() {
                cached_cstr(vm, item, b.as_bytes())?
            } else if let Some(ba) = item.downcast_ref::<PyByteArray>() {
                let data = ba.borrow_buf();
                cached_cstr(vm, item, &data)?
            } else {
                let expected = if code == b'z' {
                    "str, bytes, bytearray or None"
                } else {
                    "str, bytes or bytearray"
                };
                return arg_type_error(vm, argno, expected, item);
            };
            unsafe { *out = ptr };
            Ok(1)
        }
        b'y' if !matches!(rest.get(1), Some(b'#' | b'*')) => {
            let out: *mut *const c_char = slots.take(vm)? as *mut _;
            let ptr = if let Some(b) = item.downcast_ref::<PyBytes>() {
                cached_cstr(vm, item, b.as_bytes())?
            } else if let Some(ba) = item.downcast_ref::<PyByteArray>() {
                let data = ba.borrow_buf();
                cached_cstr(vm, item, &data)?
            } else {
                return arg_type_error(vm, argno, "bytes or bytearray", item);
            };
            unsafe { *out = ptr };
            Ok(1)
        }
        b'u' if rest.get(1) == Some(&b'#') => {
            let out: *mut *const libc::wchar_t = slots.take(vm)? as *mut _;
            let out_len: *mut isize = slots.take(vm)? as *mut _;
            let s = item
                .downcast_ref::<PyStr>()
                .ok_or_else(|| {
                    vm.new_type_error(format!(
                        "argument {argno} must be str, not {}",
                        type_name_of(item)
                    ))
                })?
                .to_owned();
            let ptr = cached_utf16(vm, &s)?;
            let key = s.as_object().as_raw() as usize;
            let len = UTF16_CACHE
                .lock()
                .expect("UTF16_CACHE poisoned")
                .get(&key)
                .unwrap()
                .1
                .len() as isize;
            unsafe {
                *out = ptr;
                *out_len = len;
            }
            Ok(2)
        }
        b'u' if rest.get(1) != Some(&b'#') => {
            let out: *mut *const libc::wchar_t = slots.take(vm)? as *mut _;
            let s = item
                .downcast_ref::<PyStr>()
                .ok_or_else(|| {
                    vm.new_type_error(format!(
                        "argument {argno} must be str, not {}",
                        type_name_of(item)
                    ))
                })?
                .to_owned();
            let ptr = cached_utf16(vm, &s)?;
            unsafe { *out = ptr };
            Ok(1)
        }
        b'U' => {
            let out: *mut *mut PyObject = slots.take(vm)? as *mut _;
            if item.downcast_ref::<PyStr>().is_none() {
                return arg_type_error(vm, argno, "str", item);
            }
            unsafe { *out = item.as_object().as_raw().cast_mut() };
            Ok(1)
        }
        b'S' => {
            let out: *mut *mut PyObject = slots.take(vm)? as *mut _;
            if item.downcast_ref::<PyBytes>().is_none() {
                return arg_type_error(vm, argno, "bytes", item);
            }
            unsafe { *out = item.as_object().as_raw().cast_mut() };
            Ok(1)
        }
        b'b' => {
            let out: *mut i8 = slots.take(vm)? as *mut _;
            let v: i8 = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| {
                    vm.new_overflow_error("signed byte integer is less than -128 or greater than 127")
                })?;
            unsafe { *out = v };
            Ok(1)
        }
        b'B' => {
            let out: *mut u8 = slots.take(vm)? as *mut _;
            let v: u8 = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| vm.new_overflow_error("unsigned byte integer is greater than 255"))?;
            unsafe { *out = v };
            Ok(1)
        }
        b'h' => {
            let out: *mut c_short = slots.take(vm)? as *mut _;
            let v: c_short = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| {
                    vm.new_overflow_error(
                        "signed short integer is less than -32768 or greater than 32767",
                    )
                })?;
            unsafe { *out = v };
            Ok(1)
        }
        b'H' => {
            let out: *mut u16 = slots.take(vm)? as *mut _;
            let v: u16 = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| vm.new_overflow_error("unsigned short integer is greater than 65535"))?;
            unsafe { *out = v };
            Ok(1)
        }
        b'i' => {
            let out: *mut c_int = slots.take(vm)? as *mut _;
            let v: c_int = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| vm.new_overflow_error("signed integer is greater than maximum"))?;
            unsafe { *out = v };
            Ok(1)
        }
        b'I' => {
            let out: *mut u32 = slots.take(vm)? as *mut _;
            let v: u32 = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| vm.new_overflow_error("unsigned integer is greater than maximum"))?;
            unsafe { *out = v };
            Ok(1)
        }
        b'l' => {
            let out: *mut core::ffi::c_long = slots.take(vm)? as *mut _;
            let v: core::ffi::c_long = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| vm.new_overflow_error("signed long integer is greater than maximum"))?;
            unsafe { *out = v };
            Ok(1)
        }
        b'k' => {
            let out: *mut core::ffi::c_ulong = slots.take(vm)? as *mut _;
            let v: core::ffi::c_ulong = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| {
                    vm.new_overflow_error("unsigned long integer is greater than maximum")
                })?;
            unsafe { *out = v };
            Ok(1)
        }
        b'L' => {
            let out: *mut i64 = slots.take(vm)? as *mut _;
            let v: i64 = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| {
                    vm.new_overflow_error("signed long long integer is greater than maximum")
                })?;
            unsafe { *out = v };
            Ok(1)
        }
        b'K' => {
            let out: *mut u64 = slots.take(vm)? as *mut _;
            let v: u64 = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| {
                    vm.new_overflow_error("unsigned long long integer is greater than maximum")
                })?;
            unsafe { *out = v };
            Ok(1)
        }
        b'n' => {
            let out: *mut isize = slots.take(vm)? as *mut _;
            let v: isize = item
                .to_owned()
                .try_index(vm)?
                .as_bigint()
                .try_into()
                .map_err(|_| vm.new_overflow_error("ssize_t is greater than maximum"))?;
            unsafe { *out = v };
            Ok(1)
        }
        b'f' => {
            let out: *mut c_float = slots.take(vm)? as *mut _;
            let v = item.to_owned().try_float(vm)?.to_f64() as c_float;
            unsafe { *out = v };
            Ok(1)
        }
        b'd' => {
            let out: *mut c_double = slots.take(vm)? as *mut _;
            let v = item.to_owned().try_float(vm)?.to_f64();
            unsafe { *out = v };
            Ok(1)
        }
        b'D' => {
            let out: *mut Py_complex = slots.take(vm)? as *mut _;
            let c = to_complex(vm, item)?;
            unsafe {
                *out = Py_complex {
                    real: c.re,
                    imag: c.im,
                };
            }
            Ok(1)
        }
        b'c' => {
            let out: *mut u8 = slots.take(vm)? as *mut _;
            let byte = single_char_byte(vm, item, argno)?;
            unsafe { *out = byte };
            Ok(1)
        }
        b'C' => {
            let out: *mut c_int = slots.take(vm)? as *mut _;
            let cp = single_char_codepoint(vm, item, argno)?;
            unsafe { *out = cp };
            Ok(1)
        }
        b'p' => {
            let out: *mut c_int = slots.take(vm)? as *mut _;
            let v = item.to_owned().is_true(vm)?;
            unsafe { *out = v as c_int };
            Ok(1)
        }
        b'O' if rest.get(1) == Some(&b'!') => {
            let ty: *mut PyTypeObject = slots.take(vm)? as *mut _;
            let out: *mut *mut PyObject = slots.take(vm)? as *mut _;
            if ty.is_null() {
                return Err(vm.new_system_error("O! format: type object is NULL"));
            }
            let ty = unsafe { &*ty };
            if !item.fast_isinstance(ty) {
                return Err(vm.new_type_error(format!(
                    "argument {argno} must be {}, not {}",
                    ty.name(),
                    type_name_of(item)
                )));
            }
            unsafe { *out = item.as_object().as_raw().cast_mut() };
            Ok(2)
        }
        b'O' if rest.get(1) == Some(&b'&') => {
            let conv = slots.take(vm)?;
            let out = slots.take(vm)?;
            if conv == 0 {
                return Err(vm.new_system_error("O& format: converter is NULL"));
            }
            let conv: unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int =
                unsafe { core::mem::transmute(conv) };
            let rc = unsafe { conv(item.as_object().as_raw().cast_mut(), out as *mut c_void) };
            if rc == 0 {
                return match vm.take_raised_exception() {
                    Some(exc) => Err(exc),
                    None => Err(vm.new_system_error(
                        "O& converter returned 0 without setting an exception",
                    )),
                };
            }
            Ok(2)
        }
        b'O' if rest.get(1) != Some(&b'&') => {
            let out: *mut *mut PyObject = slots.take(vm)? as *mut _;
            unsafe { *out = item.as_object().as_raw().cast_mut() };
            Ok(1)
        }
        b'w' if rest.get(1) == Some(&b'#') => {
            let out: *mut *mut c_void = slots.take(vm)? as *mut _;
            let out_len: *mut isize = slots.take(vm)? as *mut _;
            if let Some(ba) = item.downcast_ref::<PyByteArray>() {
                let mut data = ba.borrow_buf_mut();
                unsafe {
                    *out = data.as_mut_ptr().cast();
                    *out_len = data.len() as isize;
                }
            } else {
                return arg_type_error(vm, argno, "read-write bytes-like object", item);
            }
            Ok(2)
        }
        b'w' if !matches!(rest.get(1), Some(b'#' | b'*')) => {
            let out: *mut *mut c_void = slots.take(vm)? as *mut _;
            if let Some(ba) = item.downcast_ref::<PyByteArray>() {
                let mut data = ba.borrow_buf_mut();
                unsafe { *out = data.as_mut_ptr().cast() };
            } else {
                return arg_type_error(vm, argno, "read-write bytes-like object", item);
            }
            Ok(1)
        }
        b't' if rest.get(1) == Some(&b'#') => {
            let out: *mut *const c_char = slots.take(vm)? as *mut _;
            let out_len: *mut isize = slots.take(vm)? as *mut _;
            let data: &[u8] = if let Some(s) = item.downcast_ref::<PyStr>() {
                let s = s.try_as_utf8(vm)?;
                s.as_str().as_bytes()
            } else if let Some(b) = item.downcast_ref::<PyBytes>() {
                b.as_bytes()
            } else if let Some(ba) = item.downcast_ref::<PyByteArray>() {
                let data = ba.borrow_buf();
                let slice = data.to_vec();
                Box::leak(slice.into_boxed_slice())
            } else {
                return arg_type_error(vm, argno, "str, bytes or bytearray", item);
            };
            unsafe {
                *out = data.as_ptr().cast();
                *out_len = data.len() as isize;
            }
            Ok(2)
        }
        b's' | b'z' | b'y' if rest.get(1) == Some(&b'*') => {
            let view: *mut Py_buffer = slots.take(vm)? as *mut _;
            let allow_none = code == b'z';
            let allow_str = code != b'y';
            fill_buffer_view(vm, item, view, false, allow_none, allow_str, argno)?;
            Ok(2)
        }
        b'w' if rest.get(1) == Some(&b'*') => {
            let view: *mut Py_buffer = slots.take(vm)? as *mut _;
            fill_buffer_view(vm, item, view, true, false, false, argno)?;
            Ok(2)
        }
        _ => Err(vm.new_system_error(format!(
            "unsupported format character '{:?}' (0x{:x})",
            code as char, code
        ))),
    }
}

fn to_complex(vm: &VirtualMachine, item: &PyObjectRef) -> PyResult<Complex64> {
    if let Some(c) = item.downcast_ref::<PyComplex>() {
        return Ok(c.to_complex());
    }
    if let Some(f) = item.downcast_ref::<PyFloat>() {
        return Ok(Complex64::new(f.to_f64(), 0.0));
    }
    if let Some(i) = item.downcast_ref::<PyInt>() {
        return Ok(Complex64::new(try_bigint_to_f64(i.as_bigint(), vm)?, 0.0));
    }
    if let Some((c, _)) = item.to_owned().try_complex(vm)? {
        return Ok(c);
    }
    Err(vm.new_type_error(format!(
        "argument must be complex, not {}",
        type_name_of(item)
    )))
}

fn single_char_byte(vm: &VirtualMachine, item: &PyObjectRef, argno: usize) -> PyResult<u8> {
    if let Some(s) = item.downcast_ref::<PyStr>() {
        let s = s.try_as_utf8(vm)?;
        let bytes = s.as_str().as_bytes();
        if bytes.len() != 1 {
            return Err(vm.new_type_error(format!(
                "argument {argno} must be a string of length 1, not {}",
                type_name_of(item)
            )));
        }
        return Ok(bytes[0]);
    }
    if let Some(b) = item.downcast_ref::<PyBytes>() {
        let bytes = b.as_bytes();
        if bytes.len() != 1 {
            return Err(vm.new_type_error(format!(
                "argument {argno} must be a bytes object of length 1, not {}",
                type_name_of(item)
            )));
        }
        return Ok(bytes[0]);
    }
    arg_type_error(vm, argno, "a string of length 1", item)
}

fn single_char_codepoint(vm: &VirtualMachine, item: &PyObjectRef, argno: usize) -> PyResult<c_int> {
    let s = item.downcast_ref::<PyStr>().ok_or_else(|| {
        vm.new_type_error(format!(
            "argument {argno} must be a string of length 1, not {}",
            type_name_of(item)
        ))
    })?;
    let mut chars = s
        .to_str()
        .ok_or_else(|| vm.new_system_error("string contains surrogates"))?
        .chars();
    let ch = chars
        .next()
        .ok_or_else(|| vm.new_type_error(format!("argument {argno} must be a string of length 1")))?;
    if chars.next().is_some() {
        return Err(vm.new_type_error(format!(
            "argument {argno} must be a string of length 1, not {}",
            type_name_of(item)
        )));
    }
    Ok(ch as u32 as c_int)
}

/// Fill a `Py_buffer` from an argument item for the `s*`/`z*`/`y*`/`w*` codes.
fn fill_buffer_view(
    vm: &VirtualMachine,
    item: &PyObjectRef,
    view: *mut Py_buffer,
    writable: bool,
    allow_none: bool,
    allow_str: bool,
    argno: usize,
) -> PyResult<()> {
    if view.is_null() {
        return Err(vm.new_system_error("buffer view is NULL"));
    }
    let view = unsafe { &mut *view };
    if allow_none && item.downcast_ref::<PyNone>().is_some() {
        *view = Py_buffer {
            buf: core::ptr::null_mut(),
            obj: core::ptr::null_mut(),
            len: 0,
            itemsize: 0,
            readonly: 0,
            ndim: 0,
            format: core::ptr::null_mut(),
            shape: core::ptr::null_mut(),
            strides: core::ptr::null_mut(),
            suboffsets: core::ptr::null_mut(),
            internal: core::ptr::null_mut(),
        };
        return Ok(());
    }
    // str is accepted by s*/z* with the raw UTF-8 bytes (no NUL needed).
    if let Some(s) = item.downcast_ref::<PyStr>() {
        if !allow_str {
            return arg_type_error(vm, argno, "bytes-like object", item);
        }
        let s = s.try_as_utf8(vm)?;
        let data = s.as_str().as_bytes();
        let obj = item.clone();
        view.buf = data.as_ptr().cast_mut().cast();
        view.obj = obj.into_raw().as_ptr();
        view.len = data.len() as isize;
        view.itemsize = 1;
        view.readonly = 1;
        view.ndim = 1;
        view.format = core::ptr::null_mut();
        view.shape = core::ptr::null_mut();
        view.strides = core::ptr::null_mut();
        view.suboffsets = core::ptr::null_mut();
        view.internal = core::ptr::null_mut();
        return Ok(());
    }
    let flags = if writable { PyBUF_WRITABLE } else { 0 } | PyBUF_FORMAT;
    let rc = unsafe {
        crate::buffer::PyObject_GetBuffer(item.as_object().as_raw().cast_mut(), view, flags)
    };
    if rc != 0 {
        return match vm.take_raised_exception() {
            Some(exc) => Err(exc),
            None => Err(vm.new_system_error("PyObject_GetBuffer failed without an exception")),
        };
    }
    if writable && view.readonly != 0 {
        return Err(vm.new_buffer_error("Object is not writable."));
    }
    Ok(())
}

/// Walk a parse format string against the cursor. `format` is the full format
/// including any trailing `:name`/`;message`.
fn parse_format(
    vm: &VirtualMachine,
    cursor: &mut Cursor,
    format: &[u8],
    slots: &mut VaSlots<'_>,
) -> PyResult<()> {
    let mut optional = false;
    let mut i = 0usize;
    while i < format.len() {
        let code = format[i];
        match code {
            b' ' => i += 1,
            b'(' => {
                let item = cursor.cur_opt().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "{}takes at most {} argument{} ({} given)",
                        fun_prefix(format),
                        cursor.top_len(),
                        plural(cursor.top_len()),
                        cursor.top_pos()
                    ))
                })?;
                let t = item.downcast_ref::<PyTuple>().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "argument {} must be tuple, not {}",
                        cursor.top_pos() + 1,
                        type_name_of(&item)
                    ))
                })?;
                cursor.push(t.as_slice().to_vec());
                i += 1;
            }
            b'[' => {
                let item = cursor.cur_opt().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "{}takes at most {} argument{} ({} given)",
                        fun_prefix(format),
                        cursor.top_len(),
                        plural(cursor.top_len()),
                        cursor.top_pos()
                    ))
                })?;
                let l = item.downcast_ref::<PyList>().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "argument {} must be list, not {}",
                        cursor.top_pos() + 1,
                        type_name_of(&item)
                    ))
                })?;
                cursor.push(l.borrow_vec().to_vec());
                i += 1;
            }
            b')' | b']' => {
                cursor.pop();
                i += 1;
            }
            b'|' => {
                optional = true;
                i += 1;
            }
            b'$' => i += 1,
            b':' | b';' => return Ok(()),
            _ => {
                let argno = cursor.top_pos() + 1;
                match cursor.cur_opt() {
                    Some(item) => {
                        cursor.advance();
                        i += convert_one(vm, code, &format[i..], &item, slots, argno)?;
                    }
                    None => {
                        if !optional {
                            return Err(vm.new_type_error(format!(
                                "{}takes at most {} argument{} ({} given)",
                                fun_prefix(format),
                                cursor.top_len(),
                                plural(cursor.top_len()),
                                cursor.top_pos()
                            )));
                        }
                        write_defaults(vm, code, &format[i..], slots)?;
                        i += 1 + extra_code_bytes(&format[i..]);
                    }
                }
            }
        }
    }
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn fun_prefix(format: &[u8]) -> String {
    if let Some(pos) = format.iter().position(|&c| c == b':')
        && let Ok(name) = core::str::from_utf8(&format[pos + 1..])
    {
        return format!("{name}() ");
    }
    "function ".to_owned()
}

/// Run a parse operation, converting the Rust result into the C convention:
/// 1 on success, 0 on failure (exception set).
fn run_parse(f: impl FnOnce(&VirtualMachine) -> PyResult<bool>) -> c_int {
    with_current_vm(|vm| match f(vm) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(e) => {
            vm.set_exception(Some(e));
            0
        }
    })
}

/// C shim entry: PyArg_ParseTuple.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_parse_tuple(
    args: *mut PyObject,
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> c_int {
    run_parse(|vm| {
        if args.is_null() || format.is_null() {
            return Err(vm.new_system_error("PyArg_ParseTuple called with NULL argument"));
        }
        let format = unsafe { CStr::from_ptr(format) }.to_bytes();
        let args = unsafe { &*args }.to_owned();
        let tuple = args
            .try_downcast_ref::<PyTuple>(vm)
            .map_err(|_| vm.new_system_error("PyArg_ParseTuple() argument 1 must be a tuple"))?;
        if std::env::var("RUSTPYTHON_TRACE").is_ok() {
            eprintln!(
                "PARSE-TUPLE: fmt={:?} nslots={} nargs={}",
                core::str::from_utf8(format).unwrap_or("?"),
                nslots,
                tuple.as_slice().len()
            );
        }
        let mut cursor = Cursor::new(tuple.as_slice().to_vec());
        let mut slots = VaSlots::new(unsafe { core::slice::from_raw_parts(slots, nslots as usize) });
        parse_format(vm, &mut cursor, format, &mut slots).map(|_| true)
    })
}

/// Count top-level conversion positions in a format string.
fn count_top_level_positions(format: &[u8]) -> usize {
    let mut count = 0usize;
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < format.len() {
        let c = format[i];
        match c {
            b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'|' | b'$' | b' ' | b':' | b';' => i += 1,
            _ => {
                if depth == 0 {
                    count += 1;
                }
                i += 1 + extra_code_bytes(&format[i..]);
            }
        }
    }
    count
}

/// C shim entry: PyArg_ParseTupleAndKeywords.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_parse_tuple_and_keywords(
    args: *mut PyObject,
    kwdict: *mut PyObject,
    format: *const c_char,
    kwlist: *const *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> c_int {
    run_parse(|vm| {
        if args.is_null() || format.is_null() || kwlist.is_null() {
            return Err(vm.new_system_error(
                "PyArg_ParseTupleAndKeywords called with NULL argument",
            ));
        }
        let format = unsafe { CStr::from_ptr(format) }.to_bytes();
        let args = unsafe { &*args }.to_owned();
        let tuple = args.try_downcast_ref::<PyTuple>(vm).map_err(|_| {
            vm.new_system_error("PyArg_ParseTupleAndKeywords() argument 1 must be a tuple")
        })?;
        let kwargs = if kwdict.is_null() {
            None
        } else {
            Some(
                unsafe { &*kwdict }
                    .to_owned()
                    .try_downcast_ref::<PyDict>(vm)
                    .map_err(|_| {
                        vm.new_system_error(
                            "PyArg_ParseTupleAndKeywords() argument 2 must be a dict or NULL",
                        )
                    })?
                    .to_owned(),
            )
        };

        // Read the keyword list. Leading entries with empty names are
        // positional-only parameters (CPython convention).
        let mut names: Vec<String> = Vec::new();
        let mut i = 0usize;
        loop {
            let name_ptr = unsafe { *kwlist.add(i) };
            if name_ptr.is_null() {
                break;
            }
            names.push(unsafe { name_ptr.try_as_str(vm) }?.to_owned());
            i += 1;
        }
        let len = names.len();

        // Count top-level format positions.
        let npos = count_top_level_positions(format);
        if len > npos {
            return Err(vm.new_system_error(format!(
                "More keyword list entries ({len}) than format specifiers ({npos})"
            )));
        }

        // Merge positional and keyword arguments. `NotImplemented` marks a
        // position that was not provided at all.
        let nargs = tuple.as_slice().len();
        let nkwargs = kwargs.as_ref().map_or(0, |d| d.items_vec().len());
        let total = nargs + nkwargs;
        if total > len {
            return Err(vm.new_type_error(format!(
                "function() takes at most {len} argument{} ({total} given)",
                plural(len)
            )));
        }
        let missing = vm.ctx.not_implemented();
        let mut combined: Vec<PyObjectRef> = vec![missing.clone(); len];
        for (i, item) in tuple.as_slice().iter().enumerate() {
            combined[i] = item.clone();
        }
        if let Some(kwargs) = &kwargs {
            for (key, value) in kwargs.items_vec() {
                let key = key
                    .downcast_ref::<PyStr>()
                    .ok_or_else(|| vm.new_type_error("keywords must be strings"))?
                    .to_string();
                let Some(slot) = names.iter().position(|n| *n == key) else {
                    return Err(vm.new_type_error(format!(
                        "function() got an unexpected keyword argument '{key}'"
                    )));
                };
                if !combined[slot].is(&missing) {
                    return Err(vm.new_type_error(format!(
                        "function() got multiple values for argument '{key}'"
                    )));
                }
                combined[slot] = value.clone();
            }
        }

        let mut cursor = Cursor::new(combined);
        parse_format_with_placeholders(vm, &mut cursor, format, &mut VaSlots::new(unsafe {
            core::slice::from_raw_parts(slots, nslots as usize)
        }), &names, &missing)
    })
}

/// Like parse_format, but `NotImplemented` placeholders mean "argument not
/// provided": required positions produce missing-argument errors.
fn parse_format_with_placeholders(
    vm: &VirtualMachine,
    cursor: &mut Cursor,
    format: &[u8],
    slots: &mut VaSlots<'_>,
    names: &[String],
    missing: &PyObjectRef,
) -> PyResult<bool> {
    let mut optional = false;
    let mut i = 0usize;
    while i < format.len() {
        let code = format[i];
        match code {
            b' ' => i += 1,
            b'(' => {
                let item = cursor.cur_opt().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "{}missing required argument '{}' (pos {})",
                        fun_prefix(format),
                        names.get(cursor.top_pos()).map_or("", String::as_str),
                        cursor.top_pos() + 1
                    ))
                })?;
                let t = item.downcast_ref::<PyTuple>().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "argument {} must be tuple, not {}",
                        cursor.top_pos() + 1,
                        type_name_of(&item)
                    ))
                })?;
                cursor.push(t.as_slice().to_vec());
                i += 1;
            }
            b'[' => {
                let item = cursor.cur_opt().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "{}missing required argument '{}' (pos {})",
                        fun_prefix(format),
                        names.get(cursor.top_pos()).map_or("", String::as_str),
                        cursor.top_pos() + 1
                    ))
                })?;
                let l = item.downcast_ref::<PyList>().ok_or_else(|| {
                    vm.new_type_error(format!(
                        "argument {} must be list, not {}",
                        cursor.top_pos() + 1,
                        type_name_of(&item)
                    ))
                })?;
                cursor.push(l.borrow_vec().to_vec());
                i += 1;
            }
            b')' | b']' => {
                cursor.pop();
                i += 1;
            }
            b'|' => {
                optional = true;
                i += 1;
            }
            b'$' => i += 1,
            b':' | b';' => return Ok(true),
            _ => {
                let argno = cursor.top_pos() + 1;
                match cursor.cur_opt() {
                    Some(item) if !item.is(missing) => {
                        cursor.advance();
                        i += convert_one(vm, code, &format[i..], &item, slots, argno)?;
                    }
                    Some(_) if !optional => {
                        return Err(vm.new_type_error(format!(
                            "{}missing required argument '{}' (pos {})",
                            fun_prefix(format),
                            names.get(cursor.top_pos()).map_or("", String::as_str),
                            cursor.top_pos() + 1
                        )));
                    }
                    Some(_) => {
                        cursor.advance();
                        write_defaults(vm, code, &format[i..], slots)?;
                        i += 1 + extra_code_bytes(&format[i..]);
                    }
                    None => {
                        if !optional {
                            return Err(vm.new_type_error(format!(
                                "{}missing required argument '{}' (pos {})",
                                fun_prefix(format),
                                names.get(cursor.top_pos()).map_or("", String::as_str),
                                cursor.top_pos() + 1
                            )));
                        }
                        write_defaults(vm, code, &format[i..], slots)?;
                        i += 1 + extra_code_bytes(&format[i..]);
                    }
                }
            }
        }
    }
    Ok(true)
}

/// C shim entry: PyArg_UnpackTuple.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_unpack_tuple(
    args: *mut PyObject,
    name: *const c_char,
    min: isize,
    max: isize,
    slots: *const usize,
    nslots: c_int,
) -> c_int {
    run_parse(|vm| {
        if args.is_null() {
            return Err(vm.new_system_error("PyArg_UnpackTuple() called with NULL args"));
        }
        let args = unsafe { &*args }.to_owned();
        let tuple = args
            .try_downcast_ref::<PyTuple>(vm)
            .map_err(|_| vm.new_system_error("PyArg_UnpackTuple() argument list is not a tuple"))?;
        let nargs = tuple.as_slice().len() as isize;
        let min = if min < 0 { 0 } else { min };
        let max = if max < 0 { nargs + max } else { max };
        if nargs < min || nargs > max {
            let fname = if name.is_null() {
                None
            } else {
                Some(unsafe { name.try_as_str(vm) }?.to_owned())
            };
            return Err(match (&fname, min == max) {
                (Some(f), true) => {
                    vm.new_type_error(format!("{f} expected {min} arguments, got {nargs}"))
                }
                (Some(f), false) => vm.new_type_error(format!(
                    "{f} expected between {min} and {max} arguments, got {nargs}"
                )),
                (None, true) => vm.new_type_error(format!("expected {min} arguments, got {nargs}")),
                (None, false) => vm.new_type_error(format!(
                    "expected between {min} and {max} arguments, got {nargs}"
                )),
            });
        }
        let slots = unsafe { core::slice::from_raw_parts(slots, nslots as usize) };
        for (i, item) in tuple.as_slice().iter().enumerate() {
            let out: *mut *mut PyObject = slots[i] as *mut _;
            unsafe { *out = item.as_object().as_raw().cast_mut() };
        }
        Ok(true)
    })
}

// ---------------------------------------------------------------------------
// Py_BuildValue and the PyObject_Call* helpers
// ---------------------------------------------------------------------------

/// CPython's Py_complex (Include/complexobject.h).
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Py_complex {
    pub real: c_double,
    pub imag: c_double,
}

enum Group {
    Tuple(Vec<PyObjectRef>),
    List(Vec<PyObjectRef>),
    /// Dict: with a ':' separator the items are split keys|values and zipped;
    /// without one, items are alternating key/value pairs (CPython semantics).
    Dict {
        items: Vec<PyObjectRef>,
        in_values: bool,
        saw_separator: bool,
    },
}

fn push_to_group(group: &mut Group, value: PyObjectRef, vm: &VirtualMachine) {
    match group {
        Group::Tuple(items) | Group::List(items) => items.push(value),
        Group::Dict { items, in_values, .. } => {
            // Only route through the values list when a ':' separator was seen;
            // otherwise items form alternating key/value pairs.
            let _ = in_values;
            items.push(value);
            let _ = vm;
        }
    }
}

/// Build a value from a Py_BuildValue format string.
fn build_value(vm: &VirtualMachine, format: &[u8], slots: &mut VaSlots<'_>) -> PyResult<PyObjectRef> {
    let mut stack: Vec<Group> = Vec::new();
    let mut top: Vec<PyObjectRef> = Vec::new();
    let mut i = 0usize;
    while i < format.len() {
        let code = format[i];
        match code {
            b' ' => i += 1,
            b'(' | b'[' | b'{' => {
                let g = if code == b'(' {
                    Group::Tuple(Vec::new())
                } else if code == b'[' {
                    Group::List(Vec::new())
                } else {
                    Group::Dict { items: Vec::new(), in_values: false, saw_separator: false }
                };
                stack.push(g);
                i += 1;
            }
            b')' | b']' | b'}' => {
                let g = stack
                    .pop()
                    .ok_or_else(|| vm.new_system_error("Py_BuildValue: unbalanced group"))?;
                let value = match g {
                    Group::Tuple(items) => vm.ctx.new_tuple(items).into(),
                    Group::List(items) => vm.ctx.new_list(items).into(),
                    Group::Dict { items, saw_separator, .. } => {
                        let d = vm.ctx.new_dict();
                        let n = items.len();
                        if saw_separator {
                            // keys | values, zipped positionally.
                            if n % 2 != 0 {
                                return Err(vm.new_system_error(
                                    "Py_BuildValue: dict format has odd number of items",
                                ));
                            }
                            let (keys, values) = items.split_at(n / 2);
                            for (k, v) in keys.iter().zip(values) {
                                d.set_item(&**k, v.clone(), vm)?;
                            }
                        } else {
                            // Alternating key/value pairs.
                            if n % 2 != 0 {
                                return Err(vm.new_system_error(
                                    "Py_BuildValue: dict format has odd number of items",
                                ));
                            }
                            for pair in items.chunks_exact(2) {
                                d.set_item(&*pair[0], pair[1].clone(), vm)?;
                            }
                        }
                        d.into()
                    }
                };
                if let Some(top_group) = stack.last_mut() {
                    push_to_group(top_group, value, vm);
                } else {
                    top.push(value);
                }
                i += 1;
            }
            b':' => {
                // Inside a dict group, ':' switches from keys to values.
                if let Some(Group::Dict { in_values, saw_separator, .. }) = stack.last_mut() {
                    *in_values = true;
                    *saw_separator = true;
                }
                i += 1;
            }
            b';' | b',' => i += 1, // separators / funcname marker: no slot
            _ => {
                let value = build_one(vm, code, &format[i..], slots)?;
                if let Some(top_group) = stack.last_mut() {
                    push_to_group(top_group, value, vm);
                } else {
                    top.push(value);
                }
                i += 1 + extra_code_bytes(&format[i..]);
            }
        }
    }
    match top.len() {
        0 => Ok(vm.ctx.none()),
        1 => Ok(top.pop().unwrap()),
        // Multiple values at the top level: wrap in a tuple, matching
        // CPython's Py_BuildValue("ss", ...) -> (a, b).
        _ => Ok(vm.ctx.new_tuple(top).into()),
    }
}

fn build_one(
    vm: &VirtualMachine,
    code: u8,
    rest: &[u8],
    slots: &mut VaSlots<'_>,
) -> PyResult<PyObjectRef> {
    let slot = slots.take(vm)?;
    match code {
        b's' | b'z' if rest.get(1) == Some(&b'#') => {
            let ptr = slot as *const u8;
            let len = slots.take(vm)?;
            if code == b'z' && ptr.is_null() {
                return Ok(vm.ctx.none());
            }
            let data = unsafe { core::slice::from_raw_parts(ptr, len) };
            let s = core::str::from_utf8(data).map_err(|_| {
                vm.new_unicode_decode_error("Py_BuildValue: invalid UTF-8 in s#")
            })?;
            Ok(vm.ctx.new_str(s).into())
        }
        b's' | b'z' if !matches!(rest.get(1), Some(b'#' | b'*')) => {
            let ptr = slot as *const c_char;
            if ptr.is_null() {
                Ok(vm.ctx.none())
            } else {
                let s = unsafe { ptr.try_as_str(vm)? };
                Ok(vm.ctx.new_str(s).into())
            }
        }
        b'y' if rest.get(1) == Some(&b'#') => {
            let ptr = slot as *const u8;
            let len = slots.take(vm)?;
            Ok(vm
                .ctx
                .new_bytes(unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec())
                .into())
        }
        b'y' if rest.get(1) != Some(&b'#') => {
            let ptr = slot as *const u8;
            if ptr.is_null() {
                return Err(vm.new_system_error("Py_BuildValue: y format with NULL"));
            }
            let len = unsafe { CStr::from_ptr(ptr.cast()).to_bytes().len() };
            Ok(vm
                .ctx
                .new_bytes(unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec())
                .into())
        }
        b'u' if rest.get(1) == Some(&b'#') => {
            let ptr = slot as *const libc::wchar_t;
            let len = slots.take(vm)?;
            if ptr.is_null() {
                return Ok(vm.ctx.none());
            }
            #[cfg(windows)]
            let units = unsafe { core::slice::from_raw_parts(ptr.cast::<u16>(), len) }.to_vec();
            #[cfg(not(windows))]
            let units = unsafe { core::slice::from_raw_parts(ptr.cast::<u32>(), len) }.to_vec();
            str_from_wide(vm, &units)
        }
        b'u' if rest.get(1) != Some(&b'#') => {
            let ptr = slot as *const libc::wchar_t;
            if ptr.is_null() {
                return Ok(vm.ctx.none());
            }
            #[cfg(windows)]
            let units: Vec<u16> = {
                let mut n = 0;
                while unsafe { *ptr.cast::<u16>().add(n) } != 0 {
                    n += 1;
                }
                unsafe { core::slice::from_raw_parts(ptr.cast::<u16>(), n) }.to_vec()
            };
            #[cfg(not(windows))]
            let units: Vec<u32> = {
                let mut n = 0;
                while unsafe { *ptr.cast::<u32>().add(n) } != 0 {
                    n += 1;
                }
                unsafe { core::slice::from_raw_parts(ptr.cast::<u32>(), n) }.to_vec()
            };
            str_from_wide(vm, &units)
        }
        b'U' => {
            let obj = unsafe { (&*(slot as *mut PyObject)).to_owned() };
            if obj.downcast_ref::<PyStr>().is_some() {
                Ok(obj)
            } else {
                obj.str(vm).map(Into::into)
            }
        }
        b'O' | b'N' if rest.get(1) != Some(&b'&') => {
            Ok(unsafe { (&*(slot as *mut PyObject)).to_owned() })
        }
        b'O' if rest.get(1) == Some(&b'&') => {
            let conv = slot;
            let value = slots.take(vm)?;
            let conv: unsafe extern "C" fn(*mut PyObject, *mut *mut PyObject) -> c_int =
                unsafe { core::mem::transmute(conv) };
            let obj = unsafe { (&*(value as *mut PyObject)).to_owned() };
            let mut out: *mut PyObject = core::ptr::null_mut();
            let rc = unsafe { conv(obj.as_object().as_raw().cast_mut(), &mut out) };
            if rc == 0 {
                return match vm.take_raised_exception() {
                    Some(exc) => Err(exc),
                    None => Err(vm.new_system_error(
                        "O& converter returned 0 without setting an exception",
                    )),
                };
            }
            if out.is_null() {
                return Err(vm.new_system_error("O& converter returned NULL"));
            }
            Ok(unsafe { (&*out).to_owned() })
        }
        b'b' | b'B' | b'h' | b'H' | b'i' | b'I' | b'l' | b'L' | b'k' | b'K' | b'n' => {
            // Variadic ints arrive zero-extended in the slot; sign-extend
            // according to the code's C type.
            let v: i64 = match code {
                b'b' => (slot as u8 as i8) as i64,
                b'h' => (slot as u16 as i16) as i64,
                b'H' => (slot as u16) as i64,
                b'i' => (slot as u32 as i32) as i64,
                b'I' => (slot as u32) as i64,
                b'l' => {
                    if core::mem::size_of::<core::ffi::c_long>() == 4 {
                        (slot as u32 as i32) as i64
                    } else {
                        slot as i64
                    }
                }
                b'k' => {
                    if core::mem::size_of::<core::ffi::c_ulong>() == 4 {
                        (slot as u32) as i64
                    } else {
                        slot as i64
                    }
                }
                _ => slot as i64, // L, K, n: 8-byte values
            };
            Ok(vm.ctx.new_int(v).into())
        }
        b'c' => {
            Ok(vm.ctx.new_bytes(vec![slot as u8]).into())
        }
        b'C' => {
            let cp = slot as u32;
            let ch = char::from_u32(cp)
                .ok_or_else(|| vm.new_value_error("Py_BuildValue: invalid Unicode ordinal"))?;
            Ok(vm.ctx.new_str(ch.to_string()).into())
        }
        b'f' | b'd' => {
            let v = f64::from_bits(slot as u64);
            Ok(vm.ctx.new_float(v).into())
        }
        b'D' => {
            let c = unsafe { &*(slot as *const Py_complex) };
            Ok(vm.ctx.new_complex(Complex64::new(c.real, c.imag)).into())
        }
        _ => Err(vm.new_system_error(format!(
            "unsupported format character '{:?}' (0x{:x})",
            code as char, code
        ))),
    }
}

#[cfg(windows)]
fn str_from_wide(vm: &VirtualMachine, units: &[u16]) -> PyResult<PyObjectRef> {
    let text = String::from_utf16(units)
        .map_err(|_| vm.new_unicode_decode_error("Py_BuildValue: invalid UTF-16 in u format"))?;
    Ok(vm.ctx.new_str(text).into())
}

#[cfg(not(windows))]
fn str_from_wide(vm: &VirtualMachine, units: &[u32]) -> PyResult<PyObjectRef> {
    let mut text = String::with_capacity(units.len());
    for &u in units {
        let ch = char::from_u32(u)
            .ok_or_else(|| vm.new_unicode_decode_error("Py_BuildValue: invalid code point"))?;
        text.push(ch);
    }
    Ok(vm.ctx.new_str(text).into())
}

/// C shim entry: Py_BuildValue.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_build_value(
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult<_> {
        if format.is_null() {
            return Err(vm.new_system_error("Py_BuildValue called with NULL format"));
        }
        let format = unsafe { CStr::from_ptr(format) }.to_bytes();
        let mut slots = VaSlots::new(unsafe { core::slice::from_raw_parts(slots, nslots as usize) });
        build_value(vm, format, &mut slots)
    })
}

fn call_with_args(
    vm: &VirtualMachine,
    callable: PyObjectRef,
    args: Vec<PyObjectRef>,
) -> PyResult<PyObjectRef> {
    callable.call(FuncArgs::from(args), vm)
}

fn built_args(
    vm: &VirtualMachine,
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> PyResult<Vec<PyObjectRef>> {
    if format.is_null() {
        return Ok(Vec::new());
    }
    let format = unsafe { CStr::from_ptr(format) }.to_bytes();
    if format.is_empty() {
        // An empty format means "no arguments" for the call helpers.
        return Ok(Vec::new());
    }
    let mut slots = VaSlots::new(unsafe { core::slice::from_raw_parts(slots, nslots as usize) });
    let value = build_value(vm, format, &mut slots)?;
    if let Some(t) = value.downcast_ref::<PyTuple>() {
        Ok(t.as_slice().to_vec())
    } else {
        Ok(vec![value])
    }
}

/// C shim entry: PyObject_CallFunction.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_call_function(
    callable: *mut PyObject,
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult<_> {
        let callable = unsafe { (&*callable).to_owned() };
        let args = built_args(vm, format, slots, nslots)?;
        call_with_args(vm, callable, args)
    })
}

/// C shim entry: PyObject_CallMethod.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_call_method(
    obj: *mut PyObject,
    name: *const c_char,
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult<_> {
        let obj = unsafe { (&*obj).to_owned() };
        if name.is_null() {
            return Err(vm.new_system_error("PyObject_CallMethod called with NULL name"));
        }
        let name = unsafe { name.try_as_str(vm) }?;
        let callable = obj.get_attr(name, vm)?;
        let args = built_args(vm, format, slots, nslots)?;
        call_with_args(vm, callable, args)
    })
}

fn objargs_to_vec(slots: *const usize, nslots: c_int) -> Vec<PyObjectRef> {
    let slots = unsafe { core::slice::from_raw_parts(slots, nslots as usize) };
    let mut args = Vec::with_capacity(slots.len());
    for &s in slots {
        args.push(unsafe { (&*(s as *mut PyObject)).to_owned() });
    }
    args
}

/// C shim entry: PyObject_CallFunctionObjArgs. The slots are `PyObject*`
/// values terminated by NULL (the terminator is not included).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_call_function_objargs(
    callable: *mut PyObject,
    slots: *const usize,
    nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult<_> {
        let callable = unsafe { (&*callable).to_owned() };
        let args = objargs_to_vec(slots, nslots);
        call_with_args(vm, callable, args)
    })
}

/// C shim entry: PyObject_CallMethodObjArgs. `name` is a `PyObject*` string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_call_method_objargs(
    obj: *mut PyObject,
    name: *mut PyObject,
    slots: *const usize,
    nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult<_> {
        let obj = unsafe { (&*obj).to_owned() };
        let name = unsafe { &*name }.try_downcast_ref::<PyStr>(vm)?;
        let callable = obj.get_attr(name, vm)?;
        let args = objargs_to_vec(slots, nslots);
        call_with_args(vm, callable, args)
    })
}

// ---------------------------------------------------------------------------
// PyErr_Format (CPython Python/errors.c)
// ---------------------------------------------------------------------------

/// The C shim's variadic entry points (shim/getargs.c). Rust never calls
/// them, but referencing them anchors the C objects into the link so their
/// /EXPORT directives reach the cdylib export table (needed for
/// ctypes.pythonapi and .pyd loading).
#[used]
static C_SHIM_ANCHORS: (
    unsafe extern "C" fn(*mut PyObject, *const c_char, ...) -> c_int,
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *const c_char, *const *const c_char, ...) -> c_int,
    unsafe extern "C" fn(*mut PyObject, *const c_char, isize, isize, ...) -> c_int,
    unsafe extern "C" fn(*const c_char, ...) -> *mut PyObject,
    unsafe extern "C" fn(*mut PyObject, *const c_char, ...) -> *mut PyObject,
    unsafe extern "C" fn(*mut PyObject, *const c_char, *const c_char, ...) -> *mut PyObject,
    unsafe extern "C" fn(*mut PyObject, ...) -> *mut PyObject,
    unsafe extern "C" fn(*mut PyObject, *const c_char, ...) -> *mut PyObject,
    unsafe extern "C" fn(*mut PyObject, *const c_char, ...) -> *mut PyObject,
) = (
    PyArg_ParseTuple,
    PyArg_ParseTupleAndKeywords,
    PyArg_UnpackTuple,
    Py_BuildValue,
    PyObject_CallFunction,
    PyObject_CallMethod,
    PyObject_CallFunctionObjArgs,
    PyObject_CallMethodObjArgs,
    PyErr_Format,
);

unsafe extern "C" {
    fn PyArg_ParseTuple(args: *mut PyObject, format: *const c_char, ...) -> c_int;
    fn PyArg_ParseTupleAndKeywords(
        args: *mut PyObject,
        kwdict: *mut PyObject,
        format: *const c_char,
        kwlist: *const *const c_char,
        ...,
    ) -> c_int;
    fn PyArg_UnpackTuple(
        args: *mut PyObject,
        name: *const c_char,
        min: isize,
        max: isize,
        ...,
    ) -> c_int;
    fn Py_BuildValue(format: *const c_char, ...) -> *mut PyObject;
    fn PyObject_CallFunction(callable: *mut PyObject, format: *const c_char, ...)
        -> *mut PyObject;
    fn PyObject_CallMethod(
        obj: *mut PyObject,
        name: *const c_char,
        format: *const c_char,
        ...,
    ) -> *mut PyObject;
    fn PyObject_CallFunctionObjArgs(callable: *mut PyObject, ...) -> *mut PyObject;
    fn PyObject_CallMethodObjArgs(obj: *mut PyObject, name: *const c_char, ...)
        -> *mut PyObject;
    fn PyErr_Format(exception: *mut PyObject, format: *const c_char, ...) -> *mut PyObject;
}

fn sign_extend(v: usize, length: &str) -> i64 {
    if length.contains('l') && !length.contains("ll") {
        if core::mem::size_of::<core::ffi::c_long>() == 4 {
            (v as u32 as i32) as i64
        } else {
            v as i64
        }
    } else if length == "h" || length == "hh" {
        (v as u16 as i16) as i64
    } else if length.contains('z') || length.contains('t') || length.contains('j')
        || length.contains("ll") || length.contains('q') || length.contains('L')
    {
        v as i64
    } else {
        (v as u32 as i32) as i64
    }
}

fn zero_extend(v: usize, length: &str) -> u64 {
    if length.contains('l') && !length.contains("ll") {
        if core::mem::size_of::<core::ffi::c_long>() == 4 {
            v as u32 as u64
        } else {
            v as u64
        }
    } else if length == "h" || length == "hh" {
        v as u16 as u64
    } else if length.contains('z') || length.contains('t') || length.contains('j')
        || length.contains("ll") || length.contains('q') || length.contains('L')
    {
        v as u64
    } else {
        v as u32 as u64
    }
}

/// Format a message from a printf-style format string (PyErr_Format subset:
/// s, d, i, u, o, x, X, c, p, R, S, U, T, A, V with l/ll/h/z/t/j length
/// modifiers; width/precision are accepted and ignored).
pub(crate) fn format_message(vm: &VirtualMachine, format: &[u8], slots: &mut VaSlots<'_>) -> PyResult<String> {
    let mut out = String::new();
    let mut i = 0usize;
    while i < format.len() {
        let c = format[i];
        if c != b'%' {
            out.push(c as char);
            i += 1;
            continue;
        }
        i += 1;
        if i >= format.len() || format[i] == b'%' {
            out.push('%');
            i += 1;
            continue;
        }
        // flags
        while i < format.len() && matches!(format[i], b'-' | b'+' | b' ' | b'#' | b'0') {
            i += 1;
        }
        // width
        if i < format.len() && format[i] == b'*' {
            let _w = slots.take(vm)?;
            i += 1;
        } else {
            while i < format.len() && format[i].is_ascii_digit() {
                i += 1;
            }
        }
        // precision
        if i < format.len() && format[i] == b'.' {
            i += 1;
            if i < format.len() && format[i] == b'*' {
                let _p = slots.take(vm)?;
                i += 1;
            } else {
                while i < format.len() && format[i].is_ascii_digit() {
                    i += 1;
                }
            }
        }
        // length modifiers
        let mut length = String::new();
        while i < format.len()
            && matches!(format[i], b'l' | b'h' | b'z' | b't' | b'j' | b'L' | b'q')
        {
            length.push(format[i] as char);
            i += 1;
        }
        if i >= format.len() {
            out.push('%');
            break;
        }
        let conv = format[i];
        i += 1;
        match conv {
            b's' => {
                let ptr = slots.take(vm)? as *const c_char;
                if ptr.is_null() {
                    out.push_str("(null)");
                } else {
                    out.push_str(unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|_| {
                        vm.new_system_error("PyErr_Format: %s argument is not valid UTF-8")
                    })?);
                }
            }
            b'd' | b'i' => {
                let v = sign_extend(slots.take(vm)?, &length);
                out.push_str(&v.to_string());
            }
            b'u' | b'o' | b'x' | b'X' => {
                let v = zero_extend(slots.take(vm)?, &length);
                match conv {
                    b'u' => out.push_str(&v.to_string()),
                    b'o' => out.push_str(&format!("{v:o}")),
                    _ => out.push_str(&format!("{v:x}")),
                }
            }
            b'c' => {
                let v = slots.take(vm)? as u8 as u32;
                if let Some(ch) = char::from_u32(v) {
                    out.push(ch);
                }
            }
            b'p' => {
                let v = slots.take(vm)?;
                out.push_str(&format!("0x{v:x}"));
            }
            b'R' | b'S' | b'A' | b'U' => {
                let obj = unsafe { (&*(slots.take(vm)? as *mut PyObject)).to_owned() };
                match conv {
                    b'R' | b'A' => out.push_str(obj.repr(vm)?.as_ref()),
                    b'S' | b'U' => out.push_str(obj.str(vm)?.as_ref()),
                    _ => {}
                }
            }
            b'T' => {
                let obj = unsafe { (&*(slots.take(vm)? as *mut PyObject)).to_owned() };
                out.push_str(&type_name_of(&obj));
            }
            b'V' => {
                let ptr = slots.take(vm)? as *const c_char;
                let obj = slots.take(vm)? as *mut PyObject;
                if !obj.is_null() {
                    let obj = unsafe { &*obj }.to_owned();
                    out.push_str(obj.str(vm)?.as_ref());
                } else if !ptr.is_null() {
                    out.push_str(unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|_| {
                        vm.new_system_error("PyErr_Format: %V argument is not valid UTF-8")
                    })?);
                } else {
                    out.push_str("(null)");
                }
            }
            _ => {
                out.push('%');
                out.push(conv as char);
            }
        }
    }
    Ok(out)
}

/// C shim entry: PyErr_Format. Sets the exception and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_format(
    exception: *mut PyObject,
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult<PyObjectRef> {
        if exception.is_null() || format.is_null() {
            return Err(vm.new_system_error("PyErr_Format called with NULL argument"));
        }
        let format = unsafe { CStr::from_ptr(format) }.to_bytes();
        let mut slots = VaSlots::new(unsafe { core::slice::from_raw_parts(slots, nslots as usize) });
        let message = format_message(vm, format, &mut slots)?;
        let exc_type = unsafe { &*exception }.try_downcast_ref::<PyType>(vm)?;
        let exc = vm.invoke_exception(exc_type, vec![vm.ctx.new_str(message).into()])?;
        Err(exc)
    })
}

/// C shim entry: PyUnicode_FromFormat. Builds a Python str from a
/// printf-style format using the slot-snapshotting mechanism.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_unicode_from_format(
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> *mut PyObject {
    with_vm(|vm| -> PyResult<PyObjectRef> {
        if format.is_null() {
            return Err(vm.new_system_error("PyUnicode_FromFormat called with NULL format"));
        }
        let format = unsafe { CStr::from_ptr(format) }.to_bytes();
        let mut slots = VaSlots::new(unsafe { core::slice::from_raw_parts(slots, nslots as usize) });
        let message = format_message(vm, format, &mut slots)?;
        Ok(vm.ctx.new_str(message).into())
    })
}

/// Rust impl of PyArg_ValidateKeywordArguments: validate keyword argument dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_arg_validate_keyword_arguments(dict: *mut PyObject) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        if dict.is_null() {
            return Ok(1);
        }
        let dict_obj = unsafe { &*dict }.to_owned();
        // Check that it's a dict
        if dict_obj.downcast_ref::<PyDict>().is_none() {
            return Err(vm.new_type_error("argument must be a dict"));
        }
        // For simplicity, we validate that all keys are strings.
        // Use the Python-level keys() iterator.
        let keys = vm.call_method(&dict_obj, "keys", ())?;
        let iter = vm.call_method(&keys, "__iter__", ())?;
        loop {
            match vm.call_method(&iter, "__next__", ()) {
                Ok(key) => {
                    if key.try_downcast_ref::<PyStr>(vm).is_err() {
                        return Err(vm.new_type_error("keywords must be strings"));
                    }
                }
                Err(e) => {
                    // Check if it's StopIteration
                    let is_stop = vm.call_method(e.as_object(), "__class__", ())
                        .map(|cls| cls.is(vm.ctx.exceptions.stop_iteration.as_object()))
                        .unwrap_or(false);
                    if is_stop {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        Ok(1)
    })
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;
    use pyo3::types::PyAnyMethods;

    use core::ffi::{CStr, c_char, c_int};

    unsafe extern "C" {
        fn PyArg_ParseTuple(args: *mut pyo3::ffi::PyObject, format: *const c_char, ...) -> c_int;
        fn PyArg_ParseTupleAndKeywords(
            args: *mut pyo3::ffi::PyObject,
            kwdict: *mut pyo3::ffi::PyObject,
            format: *const c_char,
            kwlist: *const *const c_char,
            ...,
        ) -> c_int;
        fn PyArg_UnpackTuple(
            args: *mut pyo3::ffi::PyObject,
            name: *const c_char,
            min: isize,
            max: isize,
            ...,
        ) -> c_int;
        fn Py_BuildValue(format: *const c_char, ...) -> *mut pyo3::ffi::PyObject;
        fn PyObject_CallFunction(
            callable: *mut pyo3::ffi::PyObject,
            format: *const c_char,
            ...,
        ) -> *mut pyo3::ffi::PyObject;
        fn PyObject_CallMethod(
            obj: *mut pyo3::ffi::PyObject,
            name: *const c_char,
            format: *const c_char,
            ...,
        ) -> *mut pyo3::ffi::PyObject;
        fn PyObject_CallMethodObjArgs(obj: *mut pyo3::ffi::PyObject, name: *mut pyo3::ffi::PyObject, ...) -> *mut pyo3::ffi::PyObject;
        fn PyErr_Format(exception: *mut pyo3::ffi::PyObject, format: *const c_char, ...) -> *mut pyo3::ffi::PyObject;
    }

    #[test]
    fn parse_tuple_ints_str() {
        Python::attach(|py| unsafe {
            let args = Py_BuildValue(c"(iis)".as_ptr(), 42i32, -7i32, c"hello".as_ptr());
            assert!(!args.is_null());
            let args: Py<pyo3::PyAny> = Py::from_owned_ptr(py, args as *mut pyo3::ffi::PyObject);
            let mut i: i32 = 0;
            let mut j: i32 = 0;
            let mut s: *const c_char = core::ptr::null();
            let ret = PyArg_ParseTuple(
                args.as_ptr(),
                c"iis".as_ptr(),
                &mut i as *mut i32,
                &mut j as *mut i32,
                &mut s as *mut *const c_char,
            );
            assert_eq!(ret, 1);
            assert_eq!(i, 42);
            assert_eq!(j, -7);
            assert_eq!(unsafe { CStr::from_ptr(s) }.to_str().unwrap(), "hello");
        });
    }

    #[test]
    fn parse_tuple_type_error() {
        Python::attach(|py| unsafe {
            let args = Py_BuildValue(c"(i)".as_ptr(), 42i32);
            assert!(!args.is_null());
            let args: Py<pyo3::PyAny> = Py::from_owned_ptr(py, args as *mut pyo3::ffi::PyObject);
            let mut s: *const c_char = core::ptr::null();
            let ret = PyArg_ParseTuple(args.as_ptr(), c"s".as_ptr(), &mut s as *mut *const c_char);
            assert_eq!(ret, 0);
            assert!(PyErr::occurred(py));
            let err = PyErr::take(py);
            assert!(err.is_some());
        });
    }

    #[test]
    fn parse_tuple_optional() {
        Python::attach(|py| unsafe {
            let args = Py_BuildValue(c"()".as_ptr());
            let args: Py<pyo3::PyAny> = Py::from_owned_ptr(py, args as *mut pyo3::ffi::PyObject);
            let mut i: i32 = 99;
            let mut s: *const c_char = core::ptr::null();
            let ret = PyArg_ParseTuple(
                args.as_ptr(),
                c"|is".as_ptr(),
                &mut i as *mut i32,
                &mut s as *mut *const c_char,
            );
            assert_eq!(ret, 1);
            assert_eq!(i, 0); // defaulted
            assert!(s.is_null()); // defaulted
        });
    }

    #[test]
    fn parse_tuple_and_keywords() {
        Python::attach(|py| unsafe {
            let args = Py_BuildValue(c"(i)".as_ptr(), 5i32);
            let args: Py<pyo3::PyAny> = Py::from_owned_ptr(py, args as *mut pyo3::ffi::PyObject);
            let kw = Py_BuildValue(c"{s:i}".as_ptr(), c"b".as_ptr(), 9i32);
            let kw: Py<pyo3::PyAny> = Py::from_owned_ptr(py, kw as *mut pyo3::ffi::PyObject);
            let kwlist = [c"a".as_ptr(), c"b".as_ptr(), core::ptr::null()];
            let mut a: i32 = 0;
            let mut b: i32 = 0;
            let ret = PyArg_ParseTupleAndKeywords(
                args.as_ptr(),
                kw.as_ptr(),
                c"ii".as_ptr(),
                kwlist.as_ptr(),
                &mut a as *mut i32,
                &mut b as *mut i32,
            );
            assert_eq!(ret, 1);
            assert_eq!(a, 5);
            assert_eq!(b, 9);
        });
    }

    #[test]
    fn unpack_tuple() {
        Python::attach(|py| unsafe {
            let args = Py_BuildValue(c"(ii)".as_ptr(), 1i32, 2i32);
            let args: Py<pyo3::PyAny> = Py::from_owned_ptr(py, args as *mut pyo3::ffi::PyObject);
            let mut a: *mut pyo3::ffi::PyObject = core::ptr::null_mut();
            let mut b: *mut pyo3::ffi::PyObject = core::ptr::null_mut();
            let ret = PyArg_UnpackTuple(
                args.as_ptr(),
                c"f".as_ptr(),
                2,
                2,
                &mut a as *mut *mut pyo3::ffi::PyObject,
                &mut b as *mut *mut pyo3::ffi::PyObject,
            );
            assert_eq!(ret, 1);
            assert!(!a.is_null() && !b.is_null());
            let a: Py<pyo3::PyAny> = Py::from_borrowed_ptr(py, a as *mut pyo3::ffi::PyObject);
            assert_eq!(a.extract::<i32>(py).unwrap(), 1);
        });
    }

    #[test]
    fn build_value_roundtrip() {
        Python::attach(|py| unsafe {
            let v = Py_BuildValue(c"i".as_ptr(), 42i32);
            let v: Py<pyo3::PyAny> = Py::from_owned_ptr(py, v as *mut pyo3::ffi::PyObject);
            assert_eq!(v.extract::<i32>(py).unwrap(), 42);

            let v = Py_BuildValue(c"(ss)".as_ptr(), c"a".as_ptr(), c"b".as_ptr());
            let v: Py<pyo3::PyAny> = Py::from_owned_ptr(py, v as *mut pyo3::ffi::PyObject);
            let t: Vec<String> = v.extract(py).unwrap();
            assert_eq!(t, vec!["a".to_string(), "b".to_string()]);

            let v = Py_BuildValue(c"d".as_ptr(), 2.5f64);
            let v: Py<pyo3::PyAny> = Py::from_owned_ptr(py, v as *mut pyo3::ffi::PyObject);
            assert_eq!(v.extract::<f64>(py).unwrap(), 2.5);

            let v = Py_BuildValue(c"[ii]".as_ptr(), 1i32, 2i32);
            let v: Py<pyo3::PyAny> = Py::from_owned_ptr(py, v as *mut pyo3::ffi::PyObject);
            assert_eq!(v.extract::<Vec<i32>>(py).unwrap(), vec![1, 2]);
        });
    }

    #[test]
    fn call_function_and_method() {
        Python::attach(|py| unsafe {
            let s = Py_BuildValue(c"s".as_ptr(), c"hello".as_ptr());
            let s: Py<pyo3::PyAny> = Py::from_owned_ptr(py, s as *mut pyo3::ffi::PyObject);

            let upper = PyObject_CallMethod(s.as_ptr(), c"upper".as_ptr(), c"".as_ptr());
            assert!(!upper.is_null());
            let upper: Py<pyo3::PyAny> = Py::from_owned_ptr(py, upper as *mut pyo3::ffi::PyObject);
            assert_eq!(upper.extract::<String>(py).unwrap(), "HELLO");

            let joined = PyObject_CallMethod(
                s.as_ptr(),
                c"join".as_ptr(),
                c"(O)".as_ptr(),
                Py_BuildValue(c"[ss]".as_ptr(), c"x".as_ptr(), c"y".as_ptr()),
            );
            assert!(!joined.is_null());
            let joined: Py<pyo3::PyAny> = Py::from_owned_ptr(py, joined as *mut pyo3::ffi::PyObject);
            assert_eq!(joined.extract::<String>(py).unwrap(), "xhelloy");

            let name = Py_BuildValue(c"s".as_ptr(), c"upper".as_ptr());
            let name: Py<pyo3::PyAny> = Py::from_owned_ptr(py, name as *mut pyo3::ffi::PyObject);
            let r = PyObject_CallMethodObjArgs(s.as_ptr(), name.as_ptr());
            assert!(!r.is_null());
            let r: Py<pyo3::PyAny> = Py::from_owned_ptr(py, r as *mut pyo3::ffi::PyObject);
            assert_eq!(r.extract::<String>(py).unwrap(), "HELLO");
        });
    }

    #[test]
    fn call_function_with_args() {
        Python::attach(|py| unsafe {
            let double = py
                .eval(c"lambda x: x * 2", None, None)
                .unwrap()
                .unbind();
            let f = PyObject_CallFunction(double.as_ptr(), c"i".as_ptr(), 21i32);
            assert!(!f.is_null());
            let f: Py<pyo3::PyAny> = Py::from_owned_ptr(py, f as *mut pyo3::ffi::PyObject);
            assert_eq!(f.extract::<i32>(py).unwrap(), 42);
        });
    }

    #[test]
    fn err_format() {
        Python::attach(|py| unsafe {
            use pyo3::types::PyTypeMethods;
            let exc_type = py.get_type::<pyo3::exceptions::PyValueError>();
            let r = PyErr_Format(
                exc_type.as_type_ptr() as *mut pyo3::ffi::PyObject,
                c"value is %d, expected %s".as_ptr(),
                42i32,
                c"positive".as_ptr(),
            );
            assert!(r.is_null());
            assert!(PyErr::occurred(py));
            let err = PyErr::take(py).unwrap();
            let msg: String = err
                .value(py)
                .getattr("args")
                .unwrap()
                .get_item(0)
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(msg, "value is 42, expected positive");
        });
    }
}
