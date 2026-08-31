pub(crate) use _sysconfig::module_def;

#[pymodule]
pub(crate) mod _sysconfig {
    use crate::{VirtualMachine, builtins::PyDictRef, convert::ToPyObject};

    #[pyfunction]
    fn config_vars(vm: &VirtualMachine) -> PyDictRef {
        let vars = vm.ctx.new_dict();

        // CPython-exact SOABI/EXT_SUFFIX (".cp314-win_amd64.pyd"): pip's ABI
        // tag derives from SOABI and EXT_SUFFIX must equal the first
        // extension suffix (test_sysconfig asserts both).
        #[cfg(windows)]
        {
            let soabi = crate::version::SOABI;
            vars.set_item("SOABI", soabi.to_pyobject(vm), vm).unwrap();
            vars.set_item(
                "EXT_SUFFIX",
                format!(".{soabi}.pyd").to_pyobject(vm),
                vm,
            )
            .unwrap();
            // INTERPRETER_NAME controls pip's wheel tag interpreter prefix.
            // Set to "cp" (CPython-compatible) so standard cp314-cp314-* wheels
            // are findable by pip. (sys.implementation.name is now "cpython",
            // which pip maps to "cp" via INTERPRETER_SHORT_NAMES.)
            vars.set_item("INTERPRETER_NAME", "cp".to_pyobject(vm), vm)
                .unwrap();
        }
        #[cfg(not(windows))]
        {
            vars.set_item("EXT_SUFFIX", ".so".to_pyobject(vm), vm)
                .unwrap();
            vars.set_item("SOABI", "".to_pyobject(vm), vm).unwrap();
            vars.set_item("INTERPRETER_NAME", "cp".to_pyobject(vm), vm)
                .unwrap();
        }

        vars.set_item("Py_GIL_DISABLED", (0).to_pyobject(vm), vm)
            .unwrap();
        vars.set_item("Py_DEBUG", (0).to_pyobject(vm), vm).unwrap();

        vars
    }
}
