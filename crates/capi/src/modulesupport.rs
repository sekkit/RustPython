//! Ports of CPython's internal argument-parsing helpers that clinic-generated
//! code calls directly (Python/getargs.c + pycore_modsupport.h).

use crate::util::CStrExt;
use crate::{PyObject, pystate::with_vm};
use core::ffi::{c_char, c_int};
use rustpython_vm::builtins::{PyDict, PyTuple};
use rustpython_vm::types::PyComparisonOp;
use rustpython_vm::{AsObject, PyObjectRef, PyResult, VirtualMachine};

/// CPython 3.14's `struct _PyArg_Parser` (Include/cpython/modsupport.h).
/// Field order and C layout matter: extensions initialize these statics.
#[repr(C)]
pub struct PyArgParser {
    pub format: *const c_char,
    pub keywords: *const *const c_char,
    pub fname: *const c_char,
    pub custom_msg: *const c_char,
    pub once: u8,
    pub is_kwtuple_owned: c_int,
    pub pos: c_int, // number of positional-only arguments
    pub min: c_int, // minimal number of arguments
    pub max: c_int, // maximal number of positional arguments
    pub kwtuple: *mut PyObject,
    pub next: *mut Self,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<PyArgParser>() == 72);

/// _PyArg_CheckPositional (pycore_modsupport.h): validate a positional-only
/// argument count; raise TypeError and return 0 on mismatch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_CheckPositional(
    funcname: *const c_char,
    nargs: isize,
    min: isize,
    max: isize,
) -> c_int {
    with_vm(|vm| -> c_int {
        if nargs >= min && nargs <= max {
            return 1;
        }
        let name = unsafe { funcname.try_as_str(vm) }
            .unwrap_or("<unknown>")
            .to_owned();
        let msg = if max == 0 {
            format!("{name}() takes no positional arguments")
        } else if min == max {
            format!("{name}() takes {min} positional arguments")
        } else if min == 0 {
            format!("{name}() takes at most {max} positional arguments")
        } else if max == isize::MAX {
            format!("{name}() takes at least {min} positional arguments")
        } else {
            format!("{name}() takes from {min} to {max} positional arguments")
        };
        vm.set_exception(Some(vm.new_type_error(msg)));
        0
    })
}

/// _PyArg_UnpackKeywords (Python/getargs.c): resolve positional and keyword
/// arguments from the vectorcall layout into `buf` (positionals first, then
/// keyword values in kwtuple order). Entries are borrowed references; the
/// caller's args/kwnames/kwargs stay alive for the duration of the call.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_UnpackKeywords(
    args: *const *mut PyObject,
    nargs: isize,
    kwargs: *mut PyObject,
    kwnames: *mut PyObject,
    parser: *mut PyArgParser,
    minpos: c_int,
    maxpos: c_int,
    minkw: c_int,
    varpos: c_int,
    buf: *mut *mut PyObject,
) -> *mut *mut PyObject {
    with_vm(|vm| -> PyResult<*mut *mut PyObject> {
        if parser.is_null() {
            return Err(vm.new_system_error(
                "_PyArg_UnpackKeywords called with a NULL parser",
            ));
        }
        let mut args = args;
        if args.is_null() && nargs == 0 {
            args = buf;
        }

        // parser_init: build the kwtuple lazily from the keyword name table.
        let parser_ref = unsafe { &mut *parser };
        if parser_ref.once == 0 {
            let keywords = parser_ref.keywords;
            let mut len = 0usize;
            if !keywords.is_null() {
                while !unsafe { *keywords.add(len) }.is_null() {
                    len += 1;
                }
            }
            let mut names = Vec::with_capacity(len);
            for i in 0..len {
                let kw = unsafe { core::ffi::CStr::from_ptr(*keywords.add(i)) }
                    .to_str()
                    .map_err(|_| vm.new_system_error("keyword name is not valid UTF-8"))?;
                names.push(vm.ctx.new_str(kw).into());
            }            let kwtuple = vm.ctx.new_tuple(names);
            parser_ref.kwtuple = kwtuple.as_object().as_raw().cast_mut();
            core::mem::forget(kwtuple); // the parser owns it from now on
            parser_ref.is_kwtuple_owned = 1;
            parser_ref.once = 1;
        }
        let kwtuple = unsafe { &*parser_ref.kwtuple }
            .try_downcast_ref::<PyTuple>(vm)
            .map_err(|_| vm.new_system_error("_PyArg_UnpackKeywords: bad kwtuple"))?;
        let kwtuple_len = kwtuple.as_slice().len() as c_int;
        let posonly = parser_ref.pos;
        let minposonly = minpos.min(posonly);
        let maxargs = posonly + kwtuple_len;

        // Count keyword args; with kwnames, the values follow the positionals.
        let (nkwargs, kwstack) = if !kwargs.is_null() {
            let dict = unsafe { &*kwargs }.downcast_ref::<PyDict>().ok_or_else(|| {
                vm.new_type_error("_PyArg_UnpackKeywords: kwargs is not a dict")
            })?;
            (dict.__len__() as isize, core::ptr::null())
        } else if !kwnames.is_null() {
            let names = unsafe { &*kwnames }.downcast_ref::<PyTuple>().ok_or_else(|| {
                vm.new_type_error("_PyArg_UnpackKeywords: kwnames is not a tuple")
            })?;
            (names.as_slice().len() as isize, unsafe { args.add(nargs as usize) })
        } else {
            (0isize, core::ptr::null())
        };

        let fname = if parser_ref.fname.is_null() {
            "function".to_owned()
        } else {
            unsafe { parser_ref.fname.try_as_str(vm) }
                .unwrap_or("function")
                .to_owned()
        };

        if varpos == 0 && nargs + nkwargs > maxargs as isize {
            return Err(vm.new_type_error(format!(
                "{fname}() takes at most {} argument{} ({} given)",
                maxargs,
                if maxargs == 1 { "" } else { "s" },
                nargs + nkwargs
            )));
        }
        if varpos == 0 && nargs > maxpos as isize {
            let msg = if maxpos == 0 {
                format!("{fname}() takes no positional arguments")
            } else {
                format!(
                    "{fname}() takes {} {maxpos} positional argument{} ({nargs} given)",
                    if minpos < maxpos { "at most" } else { "exactly" },
                    if maxpos == 1 { "" } else { "s" }
                )
            };
            return Err(vm.new_type_error(msg));
        }
        if nargs < minposonly as isize {
            return Err(vm.new_type_error(format!(
                "{fname}() takes {} {minposonly} positional argument{} ({nargs} given)",
                if varpos != 0 || minposonly < maxpos {
                    "at least"
                } else {
                    "exactly"
                },
                if minposonly == 1 { "" } else { "s" }
            )));
        }
        let nargs = if varpos != 0 {
            nargs.min(maxpos as isize)
        } else {
            nargs
        };

        // Copy the positional arguments.
        for i in 0..nargs as usize {
            unsafe { *buf.add(i) = *args.add(i) };
        }

        // Resolve keyword arguments in kwtuple order.
        let reqlimit = if minkw != 0 { maxpos + minkw } else { minpos };
        let mut nkwargs = nkwargs;
        let mut i = nargs.max(posonly as isize);
        while (i as c_int) < maxargs {
            let key = kwtuple.as_slice()[(i as c_int - posonly) as usize].clone();
            let current = if nkwargs > 0 {
                if !kwargs.is_null() {
                    let dict = unsafe { &*kwargs }.downcast_ref::<PyDict>().ok_or_else(|| {
                        vm.new_type_error("_PyArg_UnpackKeywords: kwargs is not a dict")
                    })?;
                    dict.get_item_opt(key.as_object(), vm)?
                } else if !kwnames.is_null() {
                    find_keyword(vm, kwnames, kwstack, &key)?
                } else {
                    None
                }
            } else if i >= reqlimit as isize {
                break;
            } else {
                None
            };
            if let Some(value) = current {
                unsafe { *buf.add(i as usize) = value.as_object().as_raw().cast_mut() };
                nkwargs -= 1;
            } else if (i as c_int) < minpos || (maxpos <= i as c_int && (i as c_int) < reqlimit) {
                return Err(vm.new_type_error(format!(
                    "{fname}() missing required argument '{}' (pos {})",
                    key.as_object().str_utf8(vm)?.as_str(),
                    i + 1
                )));
            }
            i += 1;
        }

        if nkwargs > 0 {
            // No argument may be given both by name and by position.
            for i in posonly..nargs as c_int {
                let key = kwtuple.as_slice()[(i - posonly) as usize].clone();
                let by_name = if !kwargs.is_null() {
                    let dict = unsafe { &*kwargs }.downcast_ref::<PyDict>().ok_or_else(|| {
                        vm.new_type_error("_PyArg_UnpackKeywords: kwargs is not a dict")
                    })?;
                    dict.get_item_opt(key.as_object(), vm)?.is_some()
                } else if !kwnames.is_null() {
                    find_keyword(vm, kwnames, kwstack, &key)?.is_some()
                } else {
                    false
                };
                if by_name {
                    return Err(vm.new_type_error(format!(
                        "{fname}() argument for '{}' given by name and position ({})",
                        key.as_object().str_utf8(vm)?.as_str(),
                        i + 1
                    )));
                }
            }
            unexpected_keyword_arg(vm, &fname, kwargs, kwnames)?;
        }

        Ok(buf)
    })
}

fn find_keyword(
    vm: &VirtualMachine,
    kwnames: *mut PyObject,
    kwstack: *const *mut PyObject,
    key: &PyObjectRef,
) -> PyResult<Option<PyObjectRef>> {
    let names = unsafe { &*kwnames }.downcast_ref::<PyTuple>().ok_or_else(|| {
        vm.new_type_error("_PyArg_UnpackKeywords: kwnames is not a tuple")
    })?;
    for (i, name) in names.as_slice().iter().enumerate() {
        let is_match = name
            .as_object()
            .rich_compare_bool(key.as_object(), PyComparisonOp::Eq, vm)?;
        if is_match {
            let value = unsafe { *kwstack.add(i) };
            return Ok(Some(unsafe { (&*value).to_owned() }));
        }
    }
    Ok(None)
}

fn unexpected_keyword_arg(
    vm: &VirtualMachine,
    fname: &str,
    kwargs: *mut PyObject,
    kwnames: *mut PyObject,
) -> PyResult<()> {
    let key = if !kwargs.is_null() {
        let dict = unsafe { &*kwargs }.downcast_ref::<PyDict>().ok_or_else(|| {
            vm.new_type_error("_PyArg_UnpackKeywords: kwargs is not a dict")
        })?;
        dict.next_entry(0).map(|(_, k, _)| k)
    } else if !kwnames.is_null() {
        let names = unsafe { &*kwnames }.downcast_ref::<PyTuple>().ok_or_else(|| {
            vm.new_type_error("_PyArg_UnpackKeywords: kwnames is not a tuple")
        })?;
        names.as_slice().first().cloned()
    } else {
        None
    };
    match key {
        Some(key) => Err(vm.new_type_error(format!(
            "{fname}() got an unexpected keyword argument '{}'",
            key.as_object().str_utf8(vm)?.as_str()
        ))),
        None => Err(vm.new_type_error(format!(
            "{fname}() got an unexpected keyword argument"
        ))),
    }
}

/// _PyNamespace_New (pycore_namespace.h): create a types.SimpleNamespace
/// populated from the kwds dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyNamespace_New(kwds: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| -> PyResult<_> {
        let kwds = unsafe { (&*kwds).to_owned() };
        let simple_ns = vm.import("types", 0)?.get_attr("SimpleNamespace", vm)?;
        let ns = simple_ns.call((), vm)?;
        let dict = kwds
            .downcast_ref::<PyDict>()
            .ok_or_else(|| vm.new_type_error("_PyNamespace_New: kwds is not a dict"))?;
        for (k, v) in dict {
            let name = k.as_object().str_utf8(vm)?;
            ns.set_attr(&name, v, vm)?;
        }
        Ok(ns)
    })
}
