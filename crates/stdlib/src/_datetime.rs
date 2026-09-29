//! Compatibility `_datetime` module.
//!
//! RustPython uses the complete pure-Python implementation in `_pydatetime`.
//! CPython nevertheless exposes a companion `_datetime` module, and stdlib
//! code/tests import it directly (notably for `datetime_CAPI`). This native
//! shim re-exports the same classes and provides an untracked capsule with the
//! correct name; the pointer is an opaque non-null sentinel because RustPython
//! does not expose CPython's C struct layout.

pub(crate) use _datetime::module_def;

#[pymodule]
mod _datetime {
    use crate::vm::{Py, PyObjectRef, PyResult, VirtualMachine, builtins::PyModule};

    fn module_exec(vm: &VirtualMachine, module: &Py<PyModule>) -> PyResult<()> {
        let datetime = vm.import("_pydatetime", 0)?;
        for name in [
            "date",
            "datetime",
            "time",
            "timedelta",
            "timezone",
            "tzinfo",
            "MINYEAR",
            "MAXYEAR",
            "UTC",
        ] {
            module.set_attr(name, datetime.get_attr(name, vm)?, vm)?;
        }

        // The capsule is intentionally untracked: it contains no Python
        // references, matching CPython's datetime C-API capsule semantics.
        static DATETIME_CAPI_SENTINEL: u8 = 0;
        let capsule: PyObjectRef = vm
            .ctx
            .new_capsule(
                core::ptr::addr_of!(DATETIME_CAPI_SENTINEL)
                    .cast_mut()
                    .cast(),
                Some(c"datetime.datetime_CAPI"),
                None,
            )
            .into();
        module.set_attr("datetime_CAPI", capsule, vm)?;
        Ok(())
    }
}
