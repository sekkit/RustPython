use crate::object::define_py_check;
use crate::util::CStrExt;
use crate::{PyObject, pystate::with_vm};
use core::ffi::{CStr, c_char, c_int};
use core::ptr::NonNull;
use core::slice;
use core::str;
use rustpython_vm::builtins::{PyBytesRef, PyList, PyStr, PyStrRef, PyTuple, PyUtf8StrRef};
use rustpython_vm::common::wtf8::{CodePoint, Wtf8Buf};
use rustpython_vm::convert::ToPyObject;
use rustpython_vm::{AsObject, PyObjectRef, PyResult, VirtualMachine};

define_py_check!(fn PyUnicode_Check, types.str_type);
define_py_check!(exact fn PyUnicode_CheckExact, types.str_type);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromStringAndSize(
    s: *const c_char,
    len: isize,
) -> *mut PyObject {
    with_vm(|vm| {
        let len: usize = len
            .try_into()
            .map_err(|_| vm.new_system_error("length must be non-negative"))?;

        let text = if s.is_null() {
            if len != 0 {
                return Err(vm.new_system_error(
                    "PyUnicode_FromStringAndSize called with null data and non-zero len",
                ));
            }
            ""
        } else {
            let bytes = unsafe { slice::from_raw_parts(s.cast::<u8>(), len) };
            str::from_utf8(bytes).expect("PyUnicode_FromStringAndSize got non-UTF8 data")
        };

        Ok(vm.ctx.new_str(text))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromString(s: *const c_char) -> *mut PyObject {
    with_vm(|vm| {
        let s = unsafe { s.try_as_str(vm)? };
        Ok(vm.ctx.new_str(s))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromObject(obj: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        Ok(unsafe { &*obj }
            .try_downcast_ref::<PyStr>(vm)?
            .as_object()
            .str(vm))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromOrdinal(ordinal: c_int) -> *mut PyObject {
    with_vm(|vm| {
        let ordinal: u32 = ordinal
            .try_into()
            .map_err(|_| vm.new_value_error("ordinal not in range(0x110000)"))?;
        let code_point = CodePoint::from_u32(ordinal)
            .ok_or_else(|| vm.new_value_error("ordinal not in range(0x110000)"))?;
        Ok(vm.ctx.new_str(Wtf8Buf::from_iter([code_point])))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF8AndSize(
    obj: *mut PyObject,
    size: *mut isize,
) -> *const c_char {
    with_vm(|vm| {
        let unicode = unsafe { &*obj }.try_downcast_ref::<PyStr>(vm)?;

        let str = unicode.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_AsUTF8AndSize only supports UTF-8 or ASCII strings")
        })?;

        if size.is_null() {
            // We do not support null size arguments because the returned string is not NULL terminated.
            return Err(
                vm.new_system_error("size argument to PyUnicode_AsUTF8AndSize cannot be null")
            );
        }

        unsafe { *size = str.len() as isize };
        Ok(str.as_ptr())
    })
}

/// Thread-local cache for PyUnicode_AsUTF8 NULL-terminated return value.
use std::cell::RefCell;
std::thread_local! {
    static UNICODE_UTF8_CACHE: RefCell<alloc::ffi::CString> =
        RefCell::new(alloc::ffi::CString::new("").unwrap());
}

/// PyUnicode_AsUTF8: return a pointer to the UTF-8 representation.
/// The returned pointer is valid until the next PyUnicode_AsUTF8 call in
/// the same thread (matching CPython's "valid until next call" contract).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF8(obj: *mut PyObject) -> *const c_char {
    with_vm(|vm| -> rustpython_vm::PyResult<*const c_char> {
        let unicode = unsafe { &*obj }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_AsUTF8: string is not valid UTF-8")
        })?;
        let cstr = alloc::ffi::CString::new(unicode).map_err(|_| {
            vm.new_system_error("PyUnicode_AsUTF8: string contains null byte")
        })?;
        let ptr = UNICODE_UTF8_CACHE.try_with(|cache| {
            *cache.borrow_mut() = cstr;
            cache.borrow().as_ptr()
        }).unwrap_or(core::ptr::null());
        Ok(ptr)
    })
}

fn encode_unicode(
    vm: &VirtualMachine,
    unicode: *mut PyObject,
    encoding: &str,
    errors: Option<PyUtf8StrRef>,
) -> PyResult<PyBytesRef> {
    let unicode = unsafe { &*unicode }
        .try_downcast_ref::<PyStr>(vm)?
        .to_owned();
    vm.state
        .codec_registry
        .encode_text(unicode, encoding, errors, vm)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsASCIIString(unicode: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| encode_unicode(vm, unicode, "ascii", None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsLatin1String(unicode: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| encode_unicode(vm, unicode, "latin-1", None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsRawUnicodeEscapeString(
    unicode: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| encode_unicode(vm, unicode, "raw-unicode-escape", None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF16String(unicode: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| encode_unicode(vm, unicode, "utf-16", None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF32String(unicode: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| encode_unicode(vm, unicode, "utf-32", None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUnicodeEscapeString(unicode: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| encode_unicode(vm, unicode, "unicode-escape", None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsEncodedString(
    unicode: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| {
        let encoding = unsafe { encoding.try_as_str_opt(vm) }?.unwrap_or("utf-8");
        let errors =
            unsafe { errors.try_as_str_opt(vm) }?.map(|errors| vm.ctx.new_utf8_str(errors));
        encode_unicode(vm, unicode, encoding, errors)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF8String(unicode: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| encode_unicode(vm, unicode, "utf-8", None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Decode(
    s: *const c_char,
    size: isize,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| {
        let size: usize = size
            .try_into()
            .map_err(|_| vm.new_system_error("size must be non-negative"))?;

        let bytes = if s.is_null() {
            if size != 0 {
                return Err(vm.new_system_error("decode called with null data and non-zero size"));
            }
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(s.cast::<u8>(), size) }.to_vec()
        };

        let encoding = unsafe { encoding.try_as_str_opt(vm)?.unwrap_or("utf-8") };
        let errors =
            unsafe { errors.try_as_str_opt(vm) }?.map(|errors| vm.ctx.new_utf8_str(errors));

        vm.state
            .codec_registry
            .decode_text(vm.ctx.new_bytes(bytes).into(), encoding, errors, vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeASCII(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_Decode(s, size, c"ascii".as_ptr(), errors) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeLatin1(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_Decode(s, size, c"latin-1".as_ptr(), errors) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeRawUnicodeEscape(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_Decode(s, size, c"raw-unicode-escape".as_ptr(), errors) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF7(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_Decode(s, size, c"utf-7".as_ptr(), errors) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF8(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_Decode(s, size, c"utf-8".as_ptr(), errors) }
}

/// PyUnicode_EncodeUTF8: encode a unicode string to UTF-8 bytes.
/// Returns a new bytes object (owned reference).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeUTF8(
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        let s = unsafe { &*unicode }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_EncodeUTF8: string is not valid UTF-8")
        })?;
        // errors parameter is ignored for UTF-8 encoding (all valid strings
        // can be encoded to UTF-8).
        let bytes: rustpython_vm::PyObjectRef = vm.ctx.new_bytes(s.as_bytes().to_vec()).into();
        Ok(bytes.into_raw().as_ptr())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUnicodeEscape(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    unsafe { PyUnicode_Decode(s, size, c"unicode-escape".as_ptr(), errors) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefaultAndSize(
    s: *const c_char,
    size: isize,
) -> *mut PyObject {
    with_vm(|vm| {
        let size: usize = size
            .try_into()
            .map_err(|_| vm.new_system_error("size must be non-negative"))?;

        decode_fsdefault_and_size(vm, s, size)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Concat(
    left: *mut PyObject,
    right: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        let left = unsafe { &*left }.try_downcast_ref::<PyStr>(vm)?;
        let right = unsafe { &*right }.try_downcast_ref::<PyStr>(vm)?;
        vm._add(left.as_object(), right.as_object())
    })
}

/// Rust implementation of the C shim's PyUnicode_Substring.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_unicode_substring(
    obj: *mut PyObject,
    start: isize,
    end: isize,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<rustpython_vm::PyObjectRef> {
        let s = unsafe { &*obj }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_Substring: string is not valid UTF-8")
        })?;
        let len = s.chars().count() as isize;
        let start = if start < 0 { (len + start).max(0) } else { start.min(len) };
        let end = if end < 0 { (len + end).max(0) } else { end.min(len) };
        if start >= end {
            let empty: rustpython_vm::PyObjectRef = vm.ctx.empty_str.to_owned().into();
            return Ok(empty);
        }
        let start_idx = s.chars().take(start as usize).map(|c| c.len_utf8()).sum();
        let end_idx = s.chars().take(end as usize).map(|c| c.len_utf8()).sum();
        let sub = &s[start_idx..end_idx];
        Ok(vm.ctx.new_str(sub).into())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_GetLength(unicode: *mut PyObject) -> isize {
    with_vm(|vm| {
        let unicode = unsafe { &*unicode }.try_downcast_ref::<PyStr>(vm)?;
        Ok(unicode.char_len())
    })
}

/// PyUnicode_Resize: resize a unicode string to `newsize` characters.
/// Since RustPython strings are immutable, replaces the object with a
/// zero-padded string of the new length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Resize(
    unicode: *mut *mut PyObject,
    newsize: isize,
) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        if unicode.is_null() || unsafe { (*unicode).is_null() } {
            return Err(vm.new_system_error("PyUnicode_Resize called with NULL"));
        }
        let newlen: usize = newsize
            .try_into()
            .map_err(|_| vm.new_system_error("PyUnicode_Resize: negative size"))?;
        let new_str: rustpython_vm::PyObjectRef = vm.ctx.new_str("\0".repeat(newlen)).into();
        let old = unsafe { PyObjectRef::from_raw(core::ptr::NonNull::new_unchecked(*unicode)) };
        let _ = old;
        unsafe { *unicode = new_str.into_raw().as_ptr() };
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_GetDefaultEncoding() -> *const c_char {
    c"utf-8".as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternFromString(s: *const c_char) -> *mut PyObject {
    with_vm(|vm| {
        let s = unsafe { s.try_as_str(vm)? };
        Ok(vm.ctx.intern_str(s).to_owned())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Compare(left: *mut PyObject, right: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let left = unsafe { &*left }.try_downcast_ref::<PyStr>(vm)?;
        let right = unsafe { &*right }.try_downcast_ref::<PyStr>(vm)?;
        Ok(match left.as_wtf8().cmp(right.as_wtf8()) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_CompareWithASCIIString(
    left: *mut PyObject,
    right: *const c_char,
) -> c_int {
    with_vm(|vm| {
        let left = unsafe { &*left }.try_downcast_ref::<PyStr>(vm)?;
        let right = unsafe { right.try_as_str(vm)? };
        Ok(match left.as_wtf8().cmp(right.into()) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Equal(left: *mut PyObject, right: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let left = unsafe { &*left }.try_downcast_ref::<PyStr>(vm)?;
        let right = unsafe { &*right }.try_downcast_ref::<PyStr>(vm)?;
        Ok(left.as_wtf8() == right.as_wtf8())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EqualToUTF8(
    unicode: *mut PyObject,
    string: *const c_char,
) -> c_int {
    with_vm(|vm| {
        let unicode = unsafe { &*unicode }.try_downcast_ref::<PyStr>(vm)?;
        let other = unsafe { string.try_as_str(vm)? };
        Ok(unicode.to_str().is_some_and(|s| s == other))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefault(s: *const c_char) -> *mut PyObject {
    with_vm(|vm| {
        let size = unsafe { CStr::from_ptr(s) }.to_bytes().len();
        decode_fsdefault_and_size(vm, s, size)
    })
}

pub(crate) fn decode_fsdefault_and_size(
    vm: &VirtualMachine,
    s: *const c_char,
    size: usize,
) -> PyResult<PyStrRef> {
    let bytes = if s.is_null() {
        if size != 0 {
            return Err(vm.new_system_error(
                "PyUnicode_DecodeFSDefaultAndSize called with null data and non-zero size",
            ));
        }
        &[][..]
    } else {
        unsafe { slice::from_raw_parts(s.cast::<u8>(), size) }
    };

    vm.state.codec_registry.decode_text(
        vm.ctx.new_bytes(bytes.to_vec()).into(),
        vm.fs_encoding().as_str(),
        Some(vm.fs_encode_errors().to_owned()),
        vm,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeFSDefault(unicode: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        encode_unicode(
            vm,
            unicode,
            vm.fs_encoding().as_str(),
            Some(vm.fs_encode_errors().to_owned()),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromEncodedObject(
    obj: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { &*obj };

        if obj.downcast_ref::<PyStr>().is_some() {
            return Err(vm.new_type_error("decoding str is not supported"));
        }

        let encoding = unsafe { encoding.try_as_str_opt(vm) }?.unwrap_or("utf-8");
        let errors =
            unsafe { errors.try_as_str_opt(vm) }?.map(|errors| vm.ctx.new_utf8_str(errors));

        obj.try_bytes_like(vm, |b| {
            vm.state.codec_registry.decode_text(
                vm.ctx.new_bytes(b.to_vec()).into(),
                encoding,
                errors,
                vm,
            )
        })?
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Contains(
    container: *mut PyObject,
    element: *mut PyObject,
) -> c_int {
    with_vm(|vm| {
        let container = unsafe { &*container }.try_downcast_ref::<PyStr>(vm)?;
        let element = unsafe { &*element }.try_downcast_ref::<PyStr>(vm)?;
        Ok(container.as_wtf8().contains(element.as_wtf8()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Format(
    format: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        let format = unsafe { &*format }.try_downcast_ref::<PyStr>(vm)?;
        let result = format.__mod__(unsafe { &*args }.to_owned(), vm)?;
        Ok(result.to_pyobject(vm))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_IsIdentifier(s: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let s = unsafe { &*s }.try_downcast_ref::<PyStr>(vm)?;
        Ok(s.isidentifier())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Partition(
    s: *mut PyObject,
    sep: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        let s = unsafe { &*s }.try_downcast_ref::<PyStr>(vm)?;
        let sep = unsafe { &*sep }.try_downcast_ref::<PyStr>(vm)?;
        s.partition(sep.to_owned(), vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_RPartition(
    s: *mut PyObject,
    sep: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        let s = unsafe { &*s }.try_downcast_ref::<PyStr>(vm)?;
        let sep = unsafe { &*sep }.try_downcast_ref::<PyStr>(vm)?;
        s.rpartition(sep.to_owned(), vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Translate(
    str_obj: *mut PyObject,
    table: *mut PyObject,
    _errors: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| {
        let str_obj = unsafe { &*str_obj }.try_downcast_ref::<PyStr>(vm)?;
        Ok(str_obj
            .translate(unsafe { &*table }.to_owned(), vm)?
            .to_pyobject(vm))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternInPlace(string: *mut *mut PyObject) {
    with_vm(|vm| {
        let old_str = unsafe { PyObjectRef::from_raw(NonNull::new_unchecked(*string)) }
            .downcast_exact::<PyStr>(vm)
            .expect("PyUnicode_InternInPlace called with non-string object");

        let interned: PyObjectRef = vm.ctx.intern_str(old_str).to_owned().into();

        unsafe { *string = interned.into_raw().as_ptr() }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EqualToUTF8AndSize(
    unicode: *mut PyObject,
    string: *const c_char,
    size: isize,
) -> c_int {
    with_vm(|vm| {
        let size = size.try_into().map_err(|_| {
            vm.new_system_error("Negative size passed to PyUnicode_EqualToUTF8AndSize")
        })?;

        let unicode = unsafe { &*unicode }.try_downcast_ref::<PyStr>(vm)?;
        let result = unsafe {
            let slice = slice::from_raw_parts(string as _, size);
            str::from_utf8(slice)
        }
        .ok()
        .and_then(|other| Some(unicode.to_str()? == other))
        .unwrap_or(false);

        Ok(result)
    })
}

#[cfg(windows)]
unsafe fn widechar_len(mut w: *const libc::wchar_t) -> usize {
    let mut n = 0;
    while unsafe { *w } != 0 {
        n += 1;
        w = unsafe { w.add(1) };
    }
    n
}

/// PyUnicode_FromWideChar: build a str from a wchar_t buffer (UTF-16 on
/// Windows, UCS-4 elsewhere).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromWideChar(
    w: *const libc::wchar_t,
    size: isize,
) -> *mut PyObject {
    with_vm(|vm| {
        if w.is_null() && size != 0 {
            return Err(vm.new_system_error(
                "PyUnicode_FromWideChar called with null data and non-zero size",
            ));
        }
        let size: usize = if size == -1 {
            if w.is_null() {
                0
            } else {
                unsafe { widechar_len(w) }
            }
        } else {
            size.try_into().map_err(|_| {
                vm.new_system_error("PyUnicode_FromWideChar called with negative size")
            })?
        };
        #[cfg(windows)]
        {
            let units = unsafe { slice::from_raw_parts(w.cast::<u16>(), size) };
            let text = String::from_utf16(units).map_err(|_| {
                vm.new_unicode_decode_error("PyUnicode_FromWideChar got invalid UTF-16 data")
            })?;
            Ok(vm.ctx.new_str(text))
        }
        #[cfg(not(windows))]
        {
            let units = unsafe { slice::from_raw_parts(w.cast::<u32>(), size) };
            let mut text = String::with_capacity(size);
            for &unit in units {
                let ch = char::from_u32(unit).ok_or_else(|| {
                    vm.new_unicode_decode_error("PyUnicode_FromWideChar got an invalid code point")
                })?;
                text.push(ch);
            }
            Ok(vm.ctx.new_str(text))
        }
    })
}

/// PyUnicode_AsWideChar: copy the str into a wchar_t buffer. Follows CPython:
/// a NULL buffer returns the required size including the NUL; a too-small
/// buffer is filled without a terminator; otherwise the string plus NUL is
/// written. Returns the number of wchar characters written (excluding NUL).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsWideChar(
    unicode: *mut PyObject,
    w: *mut libc::wchar_t,
    size: isize,
) -> isize {
    with_vm(|vm| -> PyResult<isize> {
        let unicode = unsafe { &*unicode }.try_downcast_ref::<PyStr>(vm)?;
        let text = unicode.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_AsWideChar only supports UTF-8 or ASCII strings")
        })?;
        #[cfg(windows)]
        let units: Vec<u16> = text.encode_utf16().collect();
        #[cfg(not(windows))]
        let units: Vec<u32> = text.chars().map(|c| c as u32).collect();
        let res = units.len() as isize;
        if w.is_null() {
            return Ok(res + 1);
        }
        if size > res {
            let n = units.len();
            unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), w.cast(), n) };
            unsafe { *w.add(n) = 0 };
            Ok(res)
        } else {
            let n = size as usize;
            unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), w.cast(), n) };
            Ok(size)
        }
    })
}

/// PyUnicode_Join: join a sequence of strings with a separator.
/// separator is a unicode string, seq is a tuple/list of strings.
/// Returns a new unicode string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Join(
    separator: *mut PyObject,
    seq: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        let sep_str = unsafe { &*separator }
            .try_downcast_ref::<PyStr>(vm)?
            .to_str()
            .ok_or_else(|| vm.new_system_error("PyUnicode_Join: separator is not a valid UTF-8 string"))?;
        let seq = unsafe { &*seq };

        // Collect the string items from the sequence.
        let items: Vec<rustpython_vm::PyObjectRef> = if let Ok(tuple) =
            seq.try_downcast_ref::<PyTuple>(vm)
        {
            tuple.iter().cloned().collect()
        } else if let Ok(list) = seq.try_downcast_ref::<PyList>(vm) {
            let list = list.borrow_vec();
            list.iter().cloned().collect()
        } else {
            return Err(vm.new_type_error("PyUnicode_Join: seq must be a tuple or list"));
        };

        // Validate all items are strings and join them with Rust's string joining.
        let mut parts: Vec<&str> = Vec::with_capacity(items.len());
        for item in &items {
            let s = item.downcast_ref::<PyStr>().ok_or_else(|| {
                vm.new_type_error("sequence item must be a string")
            })?;
            let s_str = s.to_str().ok_or_else(|| {
                vm.new_system_error("sequence item is not a valid UTF-8 string")
            })?;
            parts.push(s_str);
        }
        let joined = parts.join(sep_str);
        let result: rustpython_vm::PyObjectRef = vm.ctx.new_str(joined).into();
        Ok(result.into_raw().as_ptr())
    })
}

/// Rust implementation of the C shim's PyUnicode_Split.
/// Splits a string by a separator (maxsplit = -1 means no limit).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_unicode_split(
    obj: *mut PyObject,
    sep: *mut PyObject,
    maxsplit: isize,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<rustpython_vm::PyObjectRef> {
        let s = unsafe { &*obj }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_Split: string is not valid UTF-8")
        })?;
        let sep_str = if sep.is_null() {
            None
        } else {
            let sep_obj = unsafe { &*sep };
            if sep_obj.is(vm.ctx.none().as_object()) {
                None
            } else {
                Some(sep_obj.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
                    vm.new_system_error("PyUnicode_Split: separator is not valid UTF-8")
                })?)
            }
        };
        let parts: Vec<&str> = if let Some(sep) = sep_str {
            if maxsplit >= 0 {
                s.splitn((maxsplit + 1) as usize, sep).collect()
            } else {
                s.split(sep).collect()
            }
        } else {
            // Split on whitespace (default).
            if maxsplit >= 0 {
                s.splitn((maxsplit + 1) as usize, |c: char| c.is_whitespace()).collect()
            } else {
                s.split_whitespace().collect()
            }
        };
        let items: Vec<rustpython_vm::PyObjectRef> = parts
            .iter()
            .map(|p| vm.ctx.new_str(p.to_string()).into())
            .collect();
        Ok(vm.ctx.new_list(items).into())
    })
}

/// Rust implementation of the C shim's PyUnicode_Replace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_unicode_replace(
    obj: *mut PyObject,
    old: *mut PyObject,
    new_: *mut PyObject,
    maxreplace: isize,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<rustpython_vm::PyObjectRef> {
        let s = unsafe { &*obj }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_Replace: string is not valid UTF-8")
        })?;
        let old_s = unsafe { &*old }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_Replace: old substring is not valid UTF-8")
        })?;
        let new_s = unsafe { &*new_ }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_Replace: new substring is not valid UTF-8")
        })?;
        let result = if maxreplace >= 0 {
            s.replacen(old_s, new_s, maxreplace as usize)
        } else {
            s.replace(old_s, new_s)
        };
        Ok(vm.ctx.new_str(result).into())
    })
}

/// Rust impl of PyUnicode_Count: count occurrences of a substring.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_unicode_count(
    obj: *mut PyObject,
    sub: *mut PyObject,
    start: isize,
    end: isize,
) -> isize {
    with_vm(|vm| -> rustpython_vm::PyResult<isize> {
        let s = unsafe { &*obj }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_Count: string is not valid UTF-8")
        })?;
        let sub_s = unsafe { &*sub }.try_downcast_ref::<PyStr>(vm)?.to_str().ok_or_else(|| {
            vm.new_system_error("PyUnicode_Count: substring is not valid UTF-8")
        })?;
        let len = s.chars().count() as isize;
        let start = if start < 0 { (len + start).max(0) } else { start.min(len) };
        let end = if end < 0 { (len + end).max(0) } else { end.min(len) };
        if start >= end || sub_s.is_empty() {
            return Ok(0);
        }
        // Convert byte indices to char indices.
        let start_byte = s.chars().take(start as usize).map(|c| c.len_utf8()).sum::<usize>();
        let end_byte = s.chars().take(end as usize).map(|c| c.len_utf8()).sum::<usize>();
        let sub_str = &s[start_byte..end_byte];
        Ok(sub_str.matches(sub_s).count() as isize)
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};

    use pyo3::intern;
    use pyo3::prelude::*;
    use pyo3::types::{PyBytes, PyString, PyStringMethods};

    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn unicode() {
        Python::attach(|py| {
            let string = PyString::new(py, "Hello, World!");
            assert!(string.is_instance_of::<PyString>());
            assert_eq!(string.to_str().unwrap(), "Hello, World!");
            assert_eq!(string, "Hello, World!");
        })
    }

    #[test]
    fn intern_str() {
        Python::attach(|py| {
            let _string = intern!(py, "Hello, World!");
        })
    }

    #[test]
    fn encode_utf8_via_wrapper() {
        Python::attach(|py| {
            let s = PyString::new(py, "h\u{00E9}llo");
            let encoded = s.encode_utf8().unwrap();
            assert_eq!(encoded.as_bytes(), "h\u{00E9}llo".as_bytes());
        })
    }

    #[test]
    fn from_encoded_object_bytes() {
        Python::attach(|py| {
            let src = PyBytes::new(py, b"h\xC3\xA9llo");
            let s = PyString::from_encoded_object(src.as_any(), None, None).unwrap();
            assert_eq!(s.to_str().unwrap(), "h\u{00E9}llo");
        })
    }

    #[cfg(unix)]
    #[test]
    fn fs_default_roundtrip_non_utf8_unix() {
        Python::attach(|py| {
            let original = OsStr::from_bytes(&[b'f', b'o', 0x80]);
            let py_str = original.into_pyobject(py).unwrap();
            let roundtrip: OsString = py_str.extract().unwrap();
            assert_eq!(roundtrip.as_os_str().as_bytes(), original.as_bytes());
        })
    }

    #[test]
    fn fs_default_roundtrip_utf8() {
        Python::attach(|py| {
            let original = OsStr::new("hello.txt");
            let py_str = original.into_pyobject(py).unwrap();
            let roundtrip: OsString = py_str.extract().unwrap();
            assert_eq!(roundtrip, original);
        })
    }
}
