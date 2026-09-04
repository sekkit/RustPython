#![allow(clippy::missing_safety_doc)]

use crate::pyerrors::init_exception_statics;
use crate::pylifecycle::{MAIN_INTERP, MAIN_INTERP_PTR};
use core::sync::atomic::Ordering;
pub use rustpython_vm::PyObject;
use rustpython_vm::{Context, Interpreter};
use std::sync::MutexGuard;

extern crate alloc;

pub mod abstract_;
pub mod arg;
pub mod boolobject;
pub mod buffer;
pub mod bytearrayobject;
pub mod bytesobject;
pub mod ceval;
pub mod codecs;
pub mod complexobject;
pub mod critical_section;
pub mod crash_diag;
pub mod descrobject;
pub mod dictobject;
pub mod floatobject;
pub mod foreign;
pub mod genericaliasobject;
pub mod import;
pub mod listobject;
pub mod longobject;
pub mod memoryobject;
pub mod methodobject;
pub mod missing_api;
mod modulesupport;
pub mod moduleobject;
pub mod object;
mod objectstatics;
pub mod objimpl;
pub mod osmodule;
pub mod pycapsule;
pub mod pyerrors;
pub mod pyframe;
pub mod pylifecycle;
pub mod pymem;
pub mod pystate;
pub mod pystrcmp;
mod pytime;
pub mod refcount;
pub mod setobject;
pub mod sliceobject;
pub mod traceback;
pub mod tupleobject;
pub mod unicodeobject;
mod util;
pub mod warnings;
pub mod weakrefobject;

/// Get main interpreter of this process. Will be None if it has not been initialized yet.
pub fn get_main_interpreter() -> MutexGuard<'static, Option<Interpreter>> {
    MAIN_INTERP
        .lock()
        .expect("Failed to lock interpreter mutex")
}

/// Set the main interpreter of this process. This method will panic when there is already an
/// interpreter set.
pub fn init_main_interpreter(interpreter: Interpreter) {
    let mut interp = get_main_interpreter();
    assert!(interp.is_none(), "Main interpreter is already set");
    // Safety: Interpreter was not initialized before, so we can safely assume the statics are not used
    unsafe { init_exception_statics(&Context::genesis().exceptions) };
    *interp = Some(interpreter);
    MAIN_INTERP_PTR.store(
        interp.as_ref().unwrap() as *const _ as *mut _,
        Ordering::Release,
    );
    // The embedded binary initialises through `init_main_interpreter`, not
    // `Py_InitializeEx`, so the foreign-dispatch table (and the raw
    // CPython-type materialiser) must be registered here too.
    rustpython_vm::object::foreign_dispatch::set(
        crate::foreign::foreign_getattr as *mut (),
        crate::foreign::foreign_call as *mut (),
        crate::foreign::is_foreign_object as *mut (),
        crate::foreign::wrap_foreign_object as *mut (),
    );
    rustpython_vm::object::foreign_dispatch::set_materialize_cpy_type(
        crate::foreign::materialize_cpy_type_object as *mut (),
    );
}
