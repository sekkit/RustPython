use crate::object::define_py_check;
use crate::util::CStrExt;
use crate::{PyObject, pystate::with_vm};
use core::convert::Infallible;
use core::ffi::{c_char, c_int};
use core::ptr::NonNull;
use core::slice;
use rustpython_vm::builtins::{PyBaseException, PyTuple, PyType};
use rustpython_vm::convert::IntoObject;
use rustpython_vm::exceptions::ExceptionZoo;
use rustpython_vm::{AsObject, PyObjectRef, PyRef, PyResult};

macro_rules! define_exception_statics {
    ($( $(#[$meta:meta])* $export:ident => $exc:ident ),* $(,)?) => {
        $(
            $(#[$meta])*
            #[unsafe(no_mangle)]
            pub static mut $export: *mut PyObject = core::ptr::null_mut();
        )*

        #[allow(static_mut_refs)]
        pub(crate) unsafe fn init_exception_statics(zoo: &'static ExceptionZoo) {
            unsafe {
                $(
                    $export = zoo.$exc.as_object().as_raw().cast_mut();
                )*
            }
        }
    };
}

define_exception_statics! {
    PyExc_BaseException => base_exception_type,
    PyExc_BaseExceptionGroup => base_exception_group,
    PyExc_SystemExit => system_exit,
    PyExc_KeyboardInterrupt => keyboard_interrupt,
    PyExc_GeneratorExit => generator_exit,
    PyExc_Exception => exception_type,
    PyExc_StopIteration => stop_iteration,
    PyExc_StopAsyncIteration => stop_async_iteration,
    PyExc_ArithmeticError => arithmetic_error,
    PyExc_FloatingPointError => floating_point_error,
    PyExc_SystemError => system_error,
    PyExc_TypeError => type_error,
    PyExc_OverflowError => overflow_error,
    PyExc_ZeroDivisionError => zero_division_error,
    PyExc_AssertionError => assertion_error,
    PyExc_IndexError => index_error,
    PyExc_KeyError => key_error,
    PyExc_LookupError => lookup_error,
    PyExc_AttributeError => attribute_error,
    PyExc_BufferError => buffer_error,
    PyExc_EOFError => eof_error,
    PyExc_ImportError => import_error,
    PyExc_ModuleNotFoundError => module_not_found_error,
    PyExc_MemoryError => memory_error,
    PyExc_NameError => name_error,
    PyExc_UnboundLocalError => unbound_local_error,
    PyExc_OSError => os_error,
    PyExc_BlockingIOError => blocking_io_error,
    PyExc_ChildProcessError => child_process_error,
    PyExc_ConnectionError => connection_error,
    PyExc_BrokenPipeError => broken_pipe_error,
    PyExc_ConnectionAbortedError => connection_aborted_error,
    PyExc_ConnectionRefusedError => connection_refused_error,
    PyExc_ConnectionResetError => connection_reset_error,
    PyExc_FileExistsError => file_exists_error,
    PyExc_FileNotFoundError => file_not_found_error,
    PyExc_InterruptedError => interrupted_error,
    PyExc_IsADirectoryError => is_a_directory_error,
    PyExc_NotADirectoryError => not_a_directory_error,
    PyExc_PermissionError => permission_error,
    PyExc_ProcessLookupError => process_lookup_error,
    PyExc_TimeoutError => timeout_error,
    PyExc_ReferenceError => reference_error,
    PyExc_RuntimeError => runtime_error,
    PyExc_NotImplementedError => not_implemented_error,
    PyExc_RecursionError => recursion_error,
    PyExc_SyntaxError => syntax_error,
    PyExc_IndentationError => indentation_error,
    PyExc_TabError => tab_error,
    PyExc_ValueError => value_error,
    PyExc_UnicodeError => unicode_error,
    PyExc_UnicodeDecodeError => unicode_decode_error,
    PyExc_UnicodeEncodeError => unicode_encode_error,
    PyExc_UnicodeTranslateError => unicode_translate_error,
    PyExc_Warning => warning,
    PyExc_DeprecationWarning => deprecation_warning,
    PyExc_PendingDeprecationWarning => pending_deprecation_warning,
    PyExc_RuntimeWarning => runtime_warning,
    PyExc_SyntaxWarning => syntax_warning,
    PyExc_UserWarning => user_warning,
    PyExc_FutureWarning => future_warning,
    PyExc_ImportWarning => import_warning,
    PyExc_UnicodeWarning => unicode_warning,
    PyExc_BytesWarning => bytes_warning,
    PyExc_ResourceWarning => resource_warning,
    PyExc_EncodingWarning => encoding_warning,
}

define_py_check!(fn PyExceptionInstance_Check, exceptions.base_exception_type);

#[unsafe(no_mangle)]
pub extern "C" fn PyErr_Occurred() -> *mut PyObject {
    with_vm(|vm| {
        vm.current_exception()
            .map(|exc| exc.class().as_object().as_raw())
            .unwrap_or_default()
    })
}

/// PyErr_Clear: clear the current exception (if any).
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_Clear() {
    with_vm(|vm| {
        vm.set_exception(None);
    })
}

/// PyErr_Fetch: return (type, value, traceback) of the current exception and
/// clear it. Each output is a new reference (caller must decref).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Fetch(
    ptype: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
    ptraceback: *mut *mut PyObject,
) {
    with_vm(|vm| {
        let exc = vm.take_raised_exception();
        let (type_, value, traceback) = match exc {
            Some(exc) => {
                let type_obj = exc.class().as_object().to_owned();
                let value_obj: PyObjectRef = exc.into_object();
                let tb = value_obj
                    .get_attr("__traceback__", vm)
                    .ok()
                    .filter(|o| !o.is(vm.ctx.none().as_object()))
                    .unwrap_or_else(|| vm.ctx.none().to_owned());
                (type_obj, value_obj, tb)
            }
            None => (
                vm.ctx.none().to_owned(),
                vm.ctx.none().to_owned(),
                vm.ctx.none().to_owned(),
            ),
        };
        unsafe { *ptype = type_.into_raw().as_ptr() };
        unsafe { *pvalue = value.into_raw().as_ptr() };
        unsafe { *ptraceback = traceback.into_raw().as_ptr() };
    })
}

/// PyErr_Restore: set the current exception from (type, value, traceback).
/// Steals the references (caller must not use them afterwards).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Restore(
    type_: *mut PyObject,
    value: *mut PyObject,
    traceback: *mut PyObject,
) {
    with_vm(|vm| {
        let value_obj = if value.is_null() {
            None
        } else {
            Some(unsafe { PyObjectRef::from_raw(NonNull::new_unchecked(value)) })
        };
        match value_obj {
            Some(v) => {
                unsafe { vm.set_exception(Some(v.downcast_unchecked())) };
            }
            None => vm.set_exception(None),
        }
        // The type and traceback references are stolen; drop them.
        if !type_.is_null() {
            unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(type_))) };
        }
        if !traceback.is_null() {
            unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(traceback))) };
        }
    })
}

/// PyErr_Print: print the current exception (like an uncaught traceback) and
/// clear it.
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_Print() {
    with_vm(|vm| {
        if let Some(exc) = vm.take_raised_exception() {
            vm.print_exception(exc);
        }
    })
}

// Anchors so the linker keeps the error-management functions in the export table.
#[used]
static PYERR_CLEAR_ANCHOR: extern "C" fn() = PyErr_Clear;
#[used]
static PYERR_FETCH_ANCHOR: unsafe extern "C" fn(*mut *mut PyObject, *mut *mut PyObject, *mut *mut PyObject) =
    PyErr_Fetch;
#[used]
static PYERR_RESTORE_ANCHOR: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) =
    PyErr_Restore;
#[used]
static PYERR_PRINT_ANCHOR: extern "C" fn() = PyErr_Print;

/// PyErr_NoMemory: set the current exception to MemoryError.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NoMemory() {
    with_vm(|vm| {
        let exc = vm.new_exception_empty(vm.ctx.exceptions.memory_error.to_owned());
        vm.set_exception(Some(exc));
    })
}

/// Rust impl of PyErr_BadArgument: set TypeError.
#[unsafe(no_mangle)]
pub extern "C" fn rp_va_err_bad_argument() {
    with_vm(|vm| {
        let exc = vm.new_exception_msg(vm.ctx.exceptions.type_error.to_owned(), "bad argument".into());
        vm.set_exception(Some(exc));
    })
}

/// Rust impl of PyErr_BadInternalCall: set SystemError.
#[unsafe(no_mangle)]
pub extern "C" fn rp_va_err_bad_internal_call() {
    with_vm(|vm| {
        let exc = vm.new_exception_msg(vm.ctx.exceptions.system_error.to_owned(), "Bad internal call".into());
        vm.set_exception(Some(exc));
    })
}

/// Rust impl of PyErr_SetNone: set an instance of the given exception type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_set_none(exception: *mut PyObject) {
    with_vm(|vm| {
        let exc_type = unsafe { &*exception }.try_downcast_ref::<PyType>(vm)?;
        let exc = vm.new_exception_empty(exc_type.to_owned());
        vm.set_exception(Some(exc));
        Ok(())
    })
}

/// Rust impl of PyErr_GetExcInfo: return the current exception info
/// (type, value, traceback) as new references, without clearing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_get_exc_info(
    ptype: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
    ptraceback: *mut *mut PyObject,
) {
    with_vm(|vm| {
        let (t, v, tb) = if let Some(exc) = vm.current_exception() {
            let type_obj = exc.class().as_object().to_owned();
            let value_obj: PyObjectRef = exc.into_object();
            let tb = value_obj
                .get_attr("__traceback__", vm)
                .ok()
                .filter(|o| !o.is(vm.ctx.none().as_object()))
                .unwrap_or_else(|| vm.ctx.none().to_owned());
            (type_obj, value_obj, tb)
        } else {
            (
                vm.ctx.none().to_owned(),
                vm.ctx.none().to_owned(),
                vm.ctx.none().to_owned(),
            )
        };
        unsafe { *ptype = t.into_raw().as_ptr() };
        unsafe { *pvalue = v.into_raw().as_ptr() };
        unsafe { *ptraceback = tb.into_raw().as_ptr() };
    })
}

/// Rust impl of PyErr_SetExcInfo: set the current exception info.
/// Steals the references (type, value, traceback).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_set_exc_info(
    type_: *mut PyObject,
    value: *mut PyObject,
    traceback: *mut PyObject,
) {
    with_vm(|vm| {
        let value_obj = if value.is_null() {
            None
        } else {
            Some(unsafe { PyObjectRef::from_raw(NonNull::new_unchecked(value)) })
        };
        match value_obj {
            Some(v) => vm.set_exception(Some(unsafe { v.downcast_unchecked() })),
            None => vm.set_exception(None),
        }
        if !type_.is_null() {
            unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(type_))) };
        }
        if !traceback.is_null() {
            unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(traceback))) };
        }
    })
}

/// Rust impl of PyErr_NormalizeException: normalize the exception tuple
/// so that `*pvalue` is an instance of `*ptype`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_normalize_exception(
    ptype: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
    ptraceback: *mut *mut PyObject,
) {
    with_vm(|vm| {
        if ptype.is_null() || pvalue.is_null() || ptraceback.is_null() {
            return;
        }
        let type_ = unsafe { *ptype };
        let value = unsafe { *pvalue };
        if type_.is_null() || value.is_null() {
            return;
        }
        let type_obj = unsafe { &*type_ }.to_owned();
        let value_obj = unsafe { &*value }.to_owned();
        let tb_obj = if unsafe { !ptraceback.is_null() && !(*ptraceback).is_null() } {
            Some(unsafe { &*(*ptraceback) }.to_owned())
        } else {
            None
        };
        let tb = tb_obj.unwrap_or_else(|| vm.ctx.none().to_owned());
        match vm.normalize_exception(type_obj, value_obj, tb) {
            Ok(normalized) => {
                let n_type = normalized.class().as_object().to_owned();
                let n_value: PyObjectRef = normalized.into_object();
                // Drop the old references.
                unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(type_))) };
                unsafe { drop(PyObjectRef::from_raw(NonNull::new_unchecked(value))) };
                // Set the normalized values.
                unsafe { *ptype = n_type.into_raw().as_ptr() };
                unsafe { *pvalue = n_value.into_raw().as_ptr() };
                // ptraceback stays the same (or we could set it to None).
            }
            Err(_) => {
                // Normalization failed; leave the exception as-is.
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyErr_GetRaisedException() -> *mut PyObject {
    with_vm(|vm| {
        vm.take_raised_exception()
            .map(|exc| exc.into_object().into_raw().as_ptr())
            .unwrap_or_default()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetRaisedException(exc: *mut PyObject) {
    with_vm(|vm| {
        if let Some(exc) = NonNull::new(exc) {
            let exception = unsafe { PyObjectRef::from_raw(exc).downcast_unchecked() };
            vm.set_exception(Some(exception));
        } else {
            vm.set_exception(None);
        }
    })
}

/// Rust impl of PyErr_GetHandledException: return the current exception
/// being handled (like sys.exc_info()).
#[unsafe(no_mangle)]
pub extern "C" fn rp_va_err_get_handled_exception() -> *mut PyObject {
    with_vm(|vm| {
        vm.current_exception()
            .map(|exc| exc.into_object().into_raw().as_ptr())
            .unwrap_or_default()
    })
}

/// Rust impl of PyErr_SetHandledException: set the current exception
/// being handled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_set_handled_exception(exc: *mut PyObject) {
    with_vm(|vm| {
        if let Some(exc) = NonNull::new(exc) {
            let exception = unsafe { PyObjectRef::from_raw(exc).downcast_unchecked() };
            vm.set_exception(Some(exception));
        } else {
            vm.set_exception(None);
        }
    })
}

/// Rust impl of PyErr_ResourceWarning: issue a ResourceWarning.
/// Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_resource_warning(
    source: *mut PyObject,
    warning: *mut PyObject,
) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        let source_obj = unsafe { &*source }.to_owned();
        let warning_obj = unsafe { &*warning }.to_owned();
        let category = vm.ctx.exceptions.resource_warning.to_owned();
        rustpython_vm::warn::warn(warning_obj, Some(category), 1, Some(source_obj), vm)?;
        Ok(0)
    })
}

/// Rust impl of PyErr_SyntaxLocationEx: set filename/lineno/col_offset
/// attributes on the current exception.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_syntax_location_ex(
    _exception: *mut PyObject,
    filename: *const c_char,
    lineno: c_int,
    col_offset: c_int,
) {
    with_vm(|vm| {
        let Some(exc) = vm.current_exception() else {
            return;
        };
        let filename_str = if filename.is_null() {
            ""
        } else {
            unsafe { core::ffi::CStr::from_ptr(filename) }.to_str().unwrap_or("")
        };
        if let Err(e) = exc.as_object().set_attr("filename", vm.ctx.new_str(filename_str), vm) {
            vm.set_exception(Some(e));
            return;
        }
        if let Err(e) = exc.as_object().set_attr("lineno", vm.ctx.new_int(lineno), vm) {
            vm.set_exception(Some(e));
            return;
        }
        if col_offset >= 0 {
            if let Err(e) = exc.as_object().set_attr("offset", vm.ctx.new_int(col_offset), vm) {
                vm.set_exception(Some(e));
            }
        }
    })
}

/// Rust impl of PyErr_SetInterrupt: simulate a keyboard interrupt.
#[unsafe(no_mangle)]
pub extern "C" fn rp_va_err_set_interrupt() {
    rp_va_err_set_interrupt_ex(2); // SIGINT
}

/// Rust impl of PyErr_SetInterruptEx: set the interrupt flag.
#[unsafe(no_mangle)]
pub extern "C" fn rp_va_err_set_interrupt_ex(_signum: c_int) {
    with_vm(|vm| {
        let exc = vm.new_exception_empty(vm.ctx.exceptions.keyboard_interrupt.to_owned());
        vm.set_exception(Some(exc));
    })
}

/// Rust impl of PyErr_WarnFormat: issue a warning with printf-style format.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_warn_format(
    exception: *mut PyObject,
    format: *const c_char,
    slots: *const usize,
    nslots: c_int,
) -> c_int {
    with_vm(|vm| -> rustpython_vm::PyResult<c_int> {
        if format.is_null() {
            return Err(vm.new_system_error("PyErr_WarnFormat called with NULL format"));
        }
        let format_bytes = unsafe { core::ffi::CStr::from_ptr(format) }.to_bytes();
        let mut va = crate::arg::VaSlots::new(unsafe { core::slice::from_raw_parts(slots, nslots as usize) });
        let message = crate::arg::format_message(vm, format_bytes, &mut va)?;
        let category = if exception.is_null() {
            vm.ctx.exceptions.resource_warning.to_owned()
        } else {
            unsafe { &*exception }.try_downcast_ref::<PyType>(vm)?.to_owned()
        };
        rustpython_vm::warn::warn(vm.ctx.new_str(message).into(), Some(category), 1, None, vm)?;
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetObject(exception: *mut PyObject, value: *mut PyObject) {
    with_vm::<PyResult<Infallible>, _>(|vm| {
        let exc_type = unsafe { (&*exception).to_owned() };
        let exc_val = unsafe { (&*value).to_owned() };

        let normalized = vm.normalize_exception(exc_type, exc_val, vm.ctx.none())?;
        Err(normalized)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetString(exception: *mut PyObject, message: *const c_char) {
    with_vm::<PyResult<Infallible>, _>(|vm| {
        let exc_type = unsafe { &*exception }.try_downcast_ref::<PyType>(vm)?;
        let message = unsafe { message.try_as_str(vm) }?;

        let exc = vm.invoke_exception(exc_type, vec![vm.ctx.new_str(message).into_object()])?;

        Err(exc)
    })
}

/// PyErr_ExceptionMatches: is the pending exception (or the given exception
/// type) an instance/subclass of `exc`?
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_ExceptionMatches(exc: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let exc = unsafe { &*exc }.to_owned();
        let current = vm.current_exception();
        Ok(match current {
            Some(current) => {
                let current_type = current.class();
                let matches = current_type.is_subtype(
                    exc.downcast_ref::<PyType>().ok_or_else(|| {
                        vm.new_type_error("PyErr_ExceptionMatches: exc is not a type")
                    })?,
                );
                matches as c_int
            }
            None => 0,
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyErr_PrintEx(_set_sys_last_vars: c_int) {
    with_vm(|vm| {
        let exception = vm
            .take_raised_exception()
            .expect("No exception set in PyErr_PrintEx");

        vm.print_exception(exception);
    })
}

/// Rust impl of PyErr_Display: print an exception with its traceback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_display(exception: *mut PyObject) {
    with_vm(|vm| {
        if exception.is_null() {
            return;
        }
        let exc = unsafe { &*exception }.to_owned();
        if let Ok(exc) = exc.downcast::<rustpython_vm::builtins::PyBaseException>() {
            vm.print_exception(exc);
        }
    })
}

/// Rust impl of PyErr_SetImportError: set an ImportError with message,
/// name, and path attributes. Returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_va_err_set_import_error(
    msg: *mut PyObject,
    name: *mut PyObject,
    path: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<*mut PyObject> {
        let msg_obj = unsafe { &*msg }.to_owned();
        let name_obj = if name.is_null() {
            vm.ctx.none().to_owned()
        } else {
            unsafe { &*name }.to_owned()
        };
        let path_obj = if path.is_null() {
            vm.ctx.none().to_owned()
        } else {
            unsafe { &*path }.to_owned()
        };
        let exc = vm
            .invoke_exception(
                vm.ctx.exceptions.import_error,
                vec![msg_obj],
            )?;
        exc.as_object().set_attr("name", name_obj, vm)?;
        exc.as_object().set_attr("path", path_obj, vm)?;
        vm.set_exception(Some(exc));
        Ok(core::ptr::null_mut())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_DisplayException(exc: *mut PyObject) {
    with_vm(|vm| {
        let exception = unsafe { &*exc }
            .downcast_ref::<PyBaseException>()
            .expect("PyErr_DisplayException exc must be an exception instance")
            .to_owned();

        vm.print_exception(exception);
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WriteUnraisable(obj: *mut PyObject) {
    with_vm(|vm| {
        let exception = vm
            .take_raised_exception()
            .expect("No exception set in PyErr_WriteUnraisable");

        let object = unsafe { vm.unwrap_or_none(obj.as_ref().map(|obj| obj.to_owned())) };

        vm.run_unraisable(exception, None, object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyExceptionClass_Check(obj: *mut PyObject) -> c_int {
    with_vm(|vm| unsafe {
        obj.as_ref()
            .and_then(|obj| obj.downcast_ref::<PyType>())
            .is_some_and(|ty| ty.is_subtype(vm.ctx.exceptions.base_exception_type))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NewException(
    name: *const c_char,
    base: *mut PyObject,
    dict: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| -> rustpython_vm::PyResult<_> {
        let full_name = unsafe { name.try_as_str(vm) }?;
        let (module, name) = match full_name.rsplit_once('.') {
            Some((module, name)) => (module, name),
            None => ("", full_name),
        };

        let bases: Vec<PyRef<PyType>> = unsafe { base.as_ref() }
            .map(|bases| {
                if let Some(ty) = bases.downcast_ref::<PyType>() {
                    Ok(vec![ty.to_owned()])
                } else if let Some(tuple) = bases.downcast_ref::<PyTuple>() {
                    tuple
                        .iter()
                        .map(|item| item.to_owned().downcast())
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| {
                            vm.new_type_error(
                                "PyErr_NewException base tuple must contain only types",
                            )
                        })
                } else {
                    Err(vm.new_type_error(
                        "PyErr_NewException base must be a type or a tuple of types",
                    ))
                }
            })
            .transpose()?
            // CPython: a NULL base defaults to PyExc_Exception.
            .unwrap_or_else(|| vec![vm.ctx.exceptions.exception_type.to_owned()]);

        if !dict.is_null() {
            return Err(vm.new_system_error(
                "PyErr_NewException with non-null dict is not supported yet",
            ));
        }

        Ok(vm.ctx.new_exception_type(module, name, Some(bases)))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NewExceptionWithDoc(
    name: *const c_char,
    _doc: *const c_char,
    base: *mut PyObject,
    dict: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyErr_NewException(name, base, dict) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_GivenExceptionMatches(
    given: *mut PyObject,
    exc: *mut PyObject,
) -> c_int {
    with_vm(|vm| {
        let given = unsafe { &*given };
        let exc = unsafe { &*exc };

        given.is_subclass(exc, vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetTraceback(exc: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let exc = unsafe { &*exc }.try_downcast_ref::<PyBaseException>(vm)?;
        let tb = exc
            .__traceback__()
            .map(|tb| tb.into_object().into_raw().as_ptr())
            .unwrap_or_default();
        Ok(tb)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetCause(exc: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let exc = unsafe { &*exc }.try_downcast_ref::<PyBaseException>(vm)?;
        let cause = exc
            .__cause__()
            .map(|cause| cause.into_object().into_raw().as_ptr())
            .unwrap_or_default();
        Ok(cause)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetContext(exc: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let exc = unsafe { &*exc }.try_downcast_ref::<PyBaseException>(vm)?;
        let context = exc
            .__context__()
            .map(|context| context.into_object().into_raw().as_ptr())
            .unwrap_or_default();
        Ok(context)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetCause(exc: *mut PyObject, cause: *mut PyObject) {
    with_vm(|vm| {
        let exc = unsafe { &*exc }.try_downcast_ref::<PyBaseException>(vm)?;
        let cause = NonNull::new(cause)
            .map(|obj| unsafe { PyObjectRef::from_raw(obj).downcast_unchecked() });
        exc.set___cause__(cause);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetContext(exc: *mut PyObject, context: *mut PyObject) {
    with_vm(|vm| {
        let exc = unsafe { &*exc }.try_downcast_ref::<PyBaseException>(vm)?;
        let context = NonNull::new(context)
            .map(|obj| unsafe { PyObjectRef::from_raw(obj).downcast_unchecked() });
        exc.set___context__(context);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeDecodeError_Create(
    encoding: *const c_char,
    object: *const c_char,
    length: isize,
    start: isize,
    end: isize,
    reason: *const c_char,
) -> *mut PyObject {
    with_vm(|vm| {
        let encoding = unsafe { encoding.try_as_str(vm) }?;
        let reason = unsafe { reason.try_as_str(vm) }?;
        let length: usize = length
            .try_into()
            .map_err(|_| vm.new_system_error("length must be non-negative"))?;
        let start: usize = start
            .try_into()
            .map_err(|_| vm.new_system_error("start must be non-negative"))?;
        let end: usize = end
            .try_into()
            .map_err(|_| vm.new_system_error("end must be non-negative"))?;

        let bytes = if object.is_null() {
            if length != 0 {
                return Err(vm.new_system_error(
                    "PyUnicodeDecodeError_Create called with null object and non-zero length",
                ));
            }
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(object.cast::<u8>(), length) }.to_vec()
        };

        let exc = vm.new_unicode_decode_error_real(
            vm.ctx.new_str(encoding),
            vm.ctx.new_bytes(bytes),
            start,
            end,
            vm.ctx.new_str(reason),
        );
        Ok(exc)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetTraceback(exc: *mut PyObject, tb: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let exc = unsafe { &*exc }.try_downcast_ref::<PyBaseException>(vm)?;
        let traceback = unsafe { tb.as_ref() }.map(|obj| obj.to_owned());
        exc.set___traceback__(vm.unwrap_or_none(traceback), vm)
    })
}

/// PyErr_CheckSignals: RustPython has no C-level signal delivery; there is
/// never a pending signal to handle, so this always succeeds.
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_CheckSignals() -> c_int {
    0
}

#[cfg(test)]
mod tests {
    use pyo3::PyTypeInfo;
    use pyo3::create_exception;
    use pyo3::exceptions::{PyException, PyTypeError};
    use pyo3::prelude::*;

    #[test]
    fn raised_exception() {
        Python::attach(|py| {
            PyTypeError::new_err(py.None()).restore(py);
            assert!(PyErr::occurred(py));
            assert!(PyErr::take(py).is_some());
            assert!(!PyErr::occurred(py));
        })
    }

    #[test]
    fn error_is_instance() {
        Python::attach(|py| {
            let err = PyTypeError::new_err(py.None());
            assert!(err.is_instance_of::<PyTypeError>(py));
        })
    }

    #[test]
    fn new_exception_type() {
        create_exception!(my_module, MyError, PyException, "Some description.");

        Python::attach(|py| {
            let exc = MyError::new_err("This is a new exception");
            assert!(exc.is_instance_of::<MyError>(py));
            let exc_type = MyError::type_object(py);
            assert_eq!(
                exc_type.fully_qualified_name().unwrap(),
                "my_module.MyError"
            );
        })
    }
}
