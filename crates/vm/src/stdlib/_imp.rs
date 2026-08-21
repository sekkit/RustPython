use crate::builtins::{PyCode, PyStrInterned};
use crate::frozen::FrozenModule;
use crate::import::{
    CPyModuleDef, SinglePhaseCacheEntry, add_c_methods_to_object,
    ensure_extension_def_initialized, extension_cache_get, extension_cache_put,
    extension_module_def, is_extension_def, legacy_slot_ids, mark_extension_module_executed,
    set_extension_module_def, system_error_from_cause,
};
use crate::{AsObject, PyResult, VirtualMachine, builtins::PyBaseExceptionRef};
use core::borrow::Borrow;

pub(crate) use _imp::module_def;

pub(super) use crate::vm::resolve_frozen_alias;

#[cfg(feature = "threading")]
#[pymodule(sub)]
mod lock {
    use crate::{PyResult, VirtualMachine, stdlib::_thread::RawRMutex};

    static IMP_LOCK: RawRMutex = RawRMutex::INIT;

    #[pyfunction]
    fn acquire_lock(vm: &VirtualMachine) {
        // Detach while blocking on IMP_LOCK. The import lock is held across
        // bytecode by the importlib bootstrap, so its holder can be parked at a
        // safepoint mid-hold. Blocking here while attached would keep this
        // thread from honoring a stop-the-world request, so a requester could
        // wait for this thread while this thread waits for the parked holder.
        // Detaching makes the wait park-friendly.
        vm.allow_threads(acquire_lock_for_fork);
    }

    #[pyfunction]
    fn release_lock(vm: &VirtualMachine) -> PyResult<()> {
        if !IMP_LOCK.is_locked() {
            Err(vm.new_runtime_error("Global import lock not held"))
        } else {
            unsafe { IMP_LOCK.unlock() };
            Ok(())
        }
    }

    #[pyfunction]
    fn lock_held(_vm: &VirtualMachine) -> bool {
        IMP_LOCK.is_locked()
    }

    pub(super) fn acquire_lock_for_fork() {
        IMP_LOCK.lock();
    }

    #[cfg(all(unix, feature = "host_env"))]
    pub(super) fn release_lock_after_fork_parent() {
        if IMP_LOCK.is_locked() && IMP_LOCK.is_owned_by_current_thread() {
            unsafe { IMP_LOCK.unlock() };
        }
    }

    /// Reset import lock after fork() — only if held by a dead thread.
    ///
    /// `IMP_LOCK` is a reentrant mutex. If the *current* (surviving) thread
    /// held it at fork time, the child must be able to release it normally.
    /// Only reset if a now-dead thread was the owner.
    ///
    /// # Safety
    ///
    /// Must only be called from single-threaded child after fork().
    #[cfg(all(unix, feature = "host_env"))]
    pub(crate) unsafe fn reinit_after_fork() {
        if IMP_LOCK.is_locked() && !IMP_LOCK.is_owned_by_current_thread() {
            // Held by a dead thread — reset to unlocked.
            unsafe { rustpython_common::lock::zero_reinit_after_fork(&IMP_LOCK) };
        }
    }

    /// Match CPython's `_PyImport_ReInitLock()` + `_PyImport_ReleaseLock()`
    /// behavior in the post-fork child:
    /// 1) if ownership metadata is stale (dead owner / changed tid), reset;
    /// 2) if current thread owns the lock, release it.
    #[cfg(all(unix, feature = "host_env"))]
    pub(super) unsafe fn after_fork_child_reinit_and_release() {
        unsafe { reinit_after_fork() };
        if IMP_LOCK.is_locked() && IMP_LOCK.is_owned_by_current_thread() {
            unsafe { IMP_LOCK.unlock() };
        }
    }
}

/// Re-export for fork safety code in posix.rs
///
/// Runs pre-fork on a normal attached VM thread. Detach while blocking so the
/// wait honors a concurrent stop-the-world request instead of pinning this
/// thread attached on IMP_LOCK; re-attach completes before `stop_the_world`, so
/// the fork requester protocol is unaffected.
#[cfg(all(unix, feature = "threading", feature = "host_env"))]
pub(crate) fn acquire_imp_lock_for_fork(vm: &VirtualMachine) {
    vm.allow_threads(lock::acquire_lock_for_fork);
}

#[cfg(all(unix, feature = "threading", feature = "host_env"))]
pub(crate) fn release_imp_lock_after_fork_parent() {
    lock::release_lock_after_fork_parent();
}

#[cfg(all(unix, feature = "threading", feature = "host_env"))]
pub(crate) unsafe fn reinit_imp_lock_after_fork() {
    unsafe { lock::reinit_after_fork() }
}

#[cfg(all(unix, feature = "threading", feature = "host_env"))]
pub(crate) unsafe fn after_fork_child_imp_lock_release() {
    unsafe { lock::after_fork_child_reinit_and_release() }
}

#[cfg(not(feature = "threading"))]
#[pymodule(sub)]
mod lock {
    use crate::vm::VirtualMachine;
    #[pyfunction]
    pub(super) const fn acquire_lock(_vm: &VirtualMachine) {}
    #[pyfunction]
    pub(super) const fn release_lock(_vm: &VirtualMachine) {}
    #[pyfunction]
    pub(super) const fn lock_held(_vm: &VirtualMachine) -> bool {
        false
    }
}

#[allow(dead_code)]
enum FrozenError {
    BadName,  // The given module name wasn't valid.
    NotFound, // It wasn't in PyImport_FrozenModules.
    Disabled, // -X frozen_modules=off (and not essential)
    Excluded, // The PyImport_FrozenModules entry has NULL "code"
    //        (module is present but marked as unimportable, stops search).
    Invalid, // The PyImport_FrozenModules entry is bogus
             //          (eg. does not contain executable code).
}

impl FrozenError {
    fn to_pyexception(&self, mod_name: &str, vm: &VirtualMachine) -> PyBaseExceptionRef {
        let msg = match self {
            Self::BadName | Self::NotFound => format!("No such frozen object named {mod_name}"),
            Self::Disabled => format!(
                "Frozen modules are disabled and the frozen object named {mod_name} is not essential"
            ),
            Self::Excluded => format!("Excluded frozen object named {mod_name}"),
            Self::Invalid => format!("Frozen object named {mod_name} is invalid"),
        };
        vm.new_import_error(msg, vm.ctx.new_utf8_str(mod_name))
    }
}

// look_up_frozen + use_frozen in import.c
fn find_frozen(name: &str, vm: &VirtualMachine) -> Result<FrozenModule, FrozenError> {
    let frozen = vm
        .state
        .frozen
        .get(name)
        .copied()
        .ok_or(FrozenError::NotFound)?;

    // Bootstrap modules are always available regardless of override flag
    if matches!(
        name,
        "_frozen_importlib" | "_frozen_importlib_external" | "zipimport"
    ) {
        return Ok(frozen);
    }

    // use_frozen(): override > 0 → true, override < 0 → false, 0 → default (true)
    // When disabled, non-bootstrap modules are simply not found (same as look_up_frozen)
    let override_val = vm.state.override_frozen_modules.load();
    if override_val < 0 {
        return Err(FrozenError::NotFound);
    }

    Ok(frozen)
}

#[pymodule(with(lock))]
mod _imp {
    use crate::{
        AsObject, PyObjectRef, PyPayload, PyRef, PyResult, VirtualMachine,
        builtins::{PyBytesRef, PyCode, PyMemoryView, PyModule, PyStrRef, PyUtf8StrRef},
        convert::TryFromBorrowedObject,
        function::OptionalArg,
        import, version,
    };

    use super::FrozenError;

    #[pyattr]
    fn check_hash_based_pycs(vm: &VirtualMachine) -> PyStrRef {
        vm.ctx
            .new_str(vm.state.config.settings.check_hash_pycs_mode.to_string())
    }

    #[pyattr(name = "pyc_magic_number_token")]
    use version::PYC_MAGIC_NUMBER_TOKEN;

    #[pyfunction]
    fn extension_suffixes(vm: &VirtualMachine) -> Vec<PyObjectRef> {
        #[cfg(windows)]
        {
            // CPython-exact SOABI suffix (".cp314-win_amd64.pyd"): the
            // FileFinder only finds files named <name> + this suffix, and
            // wheels built for this interpreter carry it, so modules import
            // after `pip install`.
            vec![vm.ctx.new_str(format!(".{}.pyd", crate::version::SOABI)).into()]
        }
        #[cfg(not(windows))]
        {
            // .so loading is not wired up yet; an empty list keeps extension
            // importers from looking for them (and lets CPython's extension
            // test-suite modules skip themselves).
            Vec::new()
        }
    }

    #[cfg(any(unix, windows))]
    #[pyfunction]
    fn create_dynamic(spec: PyObjectRef, vm: &VirtualMachine) -> PyResult {
        let name: PyUtf8StrRef = spec.get_attr("name", vm)?.try_into_value(vm)?;
        let origin: PyUtf8StrRef = spec.get_attr("origin", vm)?.try_into_value(vm)?;

        // PEP 489 export-name encoding: the short name (after the last dot)
        // is ASCII-encoded, or punycode-encoded when it contains non-ASCII
        // characters, with '-' replaced by '_' (Python/importdl.c).
        let short = name.as_str().rsplit('.').next().unwrap_or(name.as_str());
        let (prefix, encoded) = if short.is_ascii() {
            ("PyInit", short.to_owned())
        } else {
            let short_obj = vm.ctx.new_str(short);
            let encoded_bytes = vm.call_method(short_obj.as_object(), "encode", ("punycode",))?;
            let encoded: PyBytesRef = encoded_bytes.downcast().map_err(|_| {
                vm.new_system_error("module name punycode encoding did not return bytes")
            })?;
            let encoded = String::from_utf8_lossy(encoded.as_bytes()).replace('-', "_");
            ("PyInitU", encoded)
        };
        let symbol = format!("{prefix}_{encoded}\0");

        #[cfg(windows)]
        let handle: Result<usize, _> = {
            // Use CPython's LoadLibraryEx flags: LOAD_LIBRARY_SEARCH_DEFAULT_DIRS |
            // LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR (bpo-36085). This respects
            // AddDllDirectory paths and prefers DLLs adjacent to the PYD.
            // We use extern "system" FFI to avoid a windows-sys dependency.
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn LoadLibraryExW(
                    lpFileName: *const u16,
                    hFile: *mut core::ffi::c_void,
                    dwFlags: u32,
                ) -> *mut core::ffi::c_void;
                fn GetLastError() -> u32;
            }
            const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x00001000;
            const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x00000100;
            let os_str = OsStr::new(origin.as_str());
            let wide: Vec<u16> = os_str.encode_wide().chain(Some(0)).collect();
            let hmod = unsafe {
                LoadLibraryExW(
                    wide.as_ptr(),
                    core::ptr::null_mut(),
                    LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
                )
            };
            if hmod.is_null() {
                let err = unsafe { GetLastError() };
                Err(vm.new_import_error(
                    format!("cannot load extension module '{}': LoadLibraryExW failed (error {})",
                        name.as_str(), err),
                    name.clone().into_wtf8(),
                ))
            } else {
                Ok(hmod as usize)
            }
        };
        #[cfg(all(unix, not(target_os = "wasi")))]
        let handle = rustpython_host_env::ctypes::open_library_with_mode(
            origin.as_str(),
            rustpython_host_env::ctypes::dlopen_mode(None),
        );
        let handle = handle.map_err(|_e| {
            vm.new_import_error(
                format!("cannot load extension module '{}'", name.as_str()),
                name.clone().into_wtf8(),
            )
        })?;

        #[cfg(windows)]
        let addr = {
            // Use GetProcAddress with the raw HMODULE.
            use std::ffi::CString;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetProcAddress(
                    hModule: *mut core::ffi::c_void,
                    lpProcName: *const u8,
                ) -> *mut core::ffi::c_void;
            }
            let hmod = handle as *mut core::ffi::c_void;
            let funcname = symbol.trim_end_matches('\0');
            // GetProcAddress takes ANSI (C string), not wide.
            let cname = CString::new(funcname).unwrap_or_default();
            let ptr = unsafe { GetProcAddress(hmod, cname.as_ptr().cast()) };
            if !ptr.is_null() {
                ptr as usize
            } else {
                return Err(vm.new_import_error(
                    format!("dynamic module does not define module export function ({})", funcname),
                    name.clone().into_wtf8(),
                ));
            }
        };
        #[cfg(not(windows))]
        let addr = rustpython_host_env::ctypes::lookup_function_symbol_addr(handle, symbol.as_bytes())
            .map_err(|_| {
                vm.new_import_error(
                    format!(
                        "dynamic module does not define module export function ({})",
                        symbol.trim_end_matches('\0')
                    ),
                    name.clone().into_wtf8(),
                )
            })?;
        if addr == 0 {
            return Err(vm.new_import_error(
                format!(
                    "dynamic module does not define module export function ({})",
                    symbol.trim_end_matches('\0')
                ),
                name.clone().into_wtf8(),
            ));
        }

        // Single-phase (legacy) modules are cached globally by (origin, name);
        // a second load reuses the cached module/dict (Python/import.c).
        if let Some(entry) = super::extension_cache_get(origin.as_str(), name.as_str()) {
            return super::reload_singlephase_extension(&entry, name.as_str(), vm);
        }

        let init_fn: unsafe extern "C" fn() -> *mut crate::PyObject =
            unsafe { core::mem::transmute(addr) };
        let module_ptr = unsafe { init_fn() };

        // Validate the init result like CPython's _PyImport_RunModInitFunc.
        if module_ptr.is_null() {
            return match vm.take_raised_exception() {
                Some(exc) => Err(exc),
                None => Err(vm.new_system_error(format!(
                    "initialization of {} failed without raising an exception",
                    name.as_str()
                ))),
            };
        }
        if let Some(exc) = vm.take_raised_exception() {
            // Non-NULL result with a pending exception: single-phase modules
            // don't do this, so it must be a misbehaving multi-phase init.
            return Err(super::system_error_from_cause(
                vm,
                format!(
                    "initialization of {} raised unreported exception",
                    name.as_str()
                ),
                exc,
            ));
        }

        let module_ptr_usize = module_ptr as usize;
        if super::is_extension_def(module_ptr_usize) {
            // Multi-phase init (PEP 489): build the module from its def.
            return super::build_multiphase_module(spec, &name, module_ptr, vm);
        }
        // A PyModuleDef that never went through PyModuleDef_Init has a zeroed
        // header (m_init == NULL) and is not a valid PyObject: check the type
        // word before wrapping the pointer, or the downcast below would
        // dereference a NULL vtable.
        let ob_type = unsafe { core::ptr::addr_of!((*(module_ptr as *const [usize; 2]))[1]).read() };
        if ob_type == 0 {
            return Err(vm.new_system_error(format!(
                "init function of {} returned uninitialized object",
                name.as_str()
            )));
        }
        let module_ref = unsafe { (&*module_ptr).to_owned() };
        if super::extension_module_def(vm, &module_ref).is_some() {
            // Single-phase init: remember it in the global cache.
            if prefix == "PyInitU" {
                return Err(vm.new_system_error(format!(
                    "initialization of {} did not return PyModuleDef",
                    name.as_str()
                )));
            }
            let def_ptr = super::extension_module_def(vm, &module_ref).unwrap_or(0);
            super::register_singlephase_cache(origin.as_str(), name.as_str(), module_ptr, def_ptr, init_fn as usize, vm);
            return Ok(module_ref);
        }
        Err(vm.new_system_error(format!(
            "initialization of {} did not return an extension module",
            name.as_str()
        )))
    }

    #[cfg(any(unix, windows))]
    #[pyfunction]
    fn exec_dynamic(module: PyObjectRef, vm: &VirtualMachine) -> PyResult<i32> {
        // A Py_mod_create function may return a non-module object; CPython's
        // exec_builtin_or_dynamic is a no-op for those.
        let Some(module_ref) = module.downcast_ref::<PyModule>() else {
            return Ok(0);
        };
        // The def pointer and executed-marker live in the module's own
        // __dict__, so a raw pointer recycled after a module is freed can
        // never falsely mark a new module as already executed.
        let Some(def_ptr) = super::extension_module_def(vm, &module) else {
            return Ok(0);
        };
        let def = unsafe { &*(def_ptr as *const super::CPyModuleDef) };

        // CPython skips the exec slot when per-module state is present
        // (md_state != NULL): for m_size >= 0 that state is allocated on the
        // first exec, so reload does not re-run it. m_size == -1 defs always
        // re-run (PyModule_ExecDef in Objects/moduleobject.c).
        if def.m_size >= 0 && !super::mark_extension_module_executed(vm, &module)? {
            return Ok(0);
        }

        if !def.m_slots.is_null() {
            let mut i = 0usize;
            loop {
                let slot = unsafe { &*def.m_slots.add(i) };
                if slot.slot == 0 {
                    break;
                }
                if slot.slot == super::legacy_slot_ids::Py_mod_exec {
                    let exec: unsafe extern "C" fn(*mut crate::PyObject) -> i32 =
                        unsafe { core::mem::transmute(slot.value) };
                    let rc = unsafe { exec(module.as_object().as_raw().cast_mut()) };
                    if rc != 0 {
                        return match vm.take_raised_exception() {
                            Some(exc) => Err(exc),
                            None => Err(vm.new_system_error(format!(
                                "execution of module {} failed without setting an exception",
                                super::module_name_of(module_ref, vm)
                            ))),
                        };
                    }
                    if let Some(exc) = vm.take_raised_exception() {
                        return Err(super::system_error_from_cause(
                            vm,
                            format!(
                                "execution of module {} raised unreported exception",
                                super::module_name_of(module_ref, vm)
                            ),
                            exc,
                        ));
                    }
                }
                i += 1;
            }
        }
        Ok(0)
    }

    #[pyfunction]
    fn is_builtin(name: PyUtf8StrRef, vm: &VirtualMachine) -> bool {
        vm.state.module_defs.contains_key(name.as_str())
    }

    #[pyfunction]
    fn is_frozen(name: PyUtf8StrRef, vm: &VirtualMachine) -> bool {
        super::find_frozen(name.as_str(), vm).is_ok()
    }

    #[pyfunction]
    fn create_builtin(spec: PyObjectRef, vm: &VirtualMachine) -> PyResult {
        let sys_modules = vm.sys_module.get_attr("modules", vm).unwrap();
        let name: PyUtf8StrRef = spec.get_attr("name", vm)?.try_into_value(vm)?;

        // Check sys.modules first
        if let Ok(module) = sys_modules.get_item(&*name, vm) {
            return Ok(module);
        }

        let name_str = name.as_str();
        if let Some(&def) = vm.state.module_defs.get(name_str) {
            // Phase 1: Create module (use create slot if provided, else default creation)
            let module = if let Some(create) = def.slots.create {
                // Custom module creation
                create(vm, &spec, def)?
            } else {
                // Default module creation
                PyModule::from_def(def).into_ref(&vm.ctx)
            };

            // Initialize module dict and methods
            // Corresponds to PyModule_FromDefAndSpec: md_def, _add_methods_to_object, PyModule_SetDocString
            PyModule::__init_dict_from_def(vm, &module);
            module.__init_methods(vm)?;

            // Add to sys.modules BEFORE exec (critical for circular import handling)
            sys_modules.set_item(name.as_pystr(), module.clone().into(), vm)?;

            // Phase 2: Call exec slot (can safely import other modules now)
            if let Some(exec) = def.slots.exec {
                exec(vm, &module)?;
            }

            return Ok(module.into());
        }

        Ok(vm.ctx.none())
    }

    #[pyfunction]
    fn exec_builtin(_mod: PyRef<PyModule>) -> i32 {
        // For multi-phase init modules, exec is already called in create_builtin
        0
    }

    #[pyfunction]
    fn get_frozen_object(
        name: PyUtf8StrRef,
        data: OptionalArg<PyObjectRef>,
        vm: &VirtualMachine,
    ) -> PyResult<PyRef<PyCode>> {
        if let OptionalArg::Present(data) = data
            && !vm.is_none(&data)
        {
            let invalid_err = || {
                vm.new_import_error(
                    format!("Frozen object named '{}' is invalid", name.as_str()),
                    name.clone().into_wtf8(),
                )
            };
            // A non-buffer is a TypeError, not invalid frozen data.
            crate::protocol::PyBuffer::try_from_borrowed_object(vm, &data)?;
            // The data is a marshalled code object: a whole marshal value, which
            // deserialize_code() does not read — it takes the code body alone,
            // without the type byte the writer puts in front of it.
            let loads = vm.import("marshal", 0)?.get_attr("loads", vm)?;
            let code = loads.call((data,), vm).map_err(|_| invalid_err())?;
            return code.downcast::<PyCode>().map_err(|_| invalid_err());
        }
        import::make_frozen(vm, name.as_str())
    }

    #[pyfunction]
    fn init_frozen(name: PyUtf8StrRef, vm: &VirtualMachine) -> PyResult {
        import::import_frozen(vm, name.as_str())
    }

    #[pyfunction]
    fn is_frozen_package(name: PyUtf8StrRef, vm: &VirtualMachine) -> PyResult<bool> {
        let name_str = name.as_str();
        super::find_frozen(name_str, vm)
            .map(|frozen| frozen.package)
            .map_err(|e| e.to_pyexception(name_str, vm))
    }

    #[pyfunction]
    fn _override_frozen_modules_for_tests(value: isize, vm: &VirtualMachine) {
        vm.state.override_frozen_modules.store(value);
    }

    #[pyfunction]
    fn _fix_co_filename(code: PyRef<PyCode>, path: PyStrRef, vm: &VirtualMachine) {
        let old_name = code.source_path();
        let new_name = vm.ctx.intern_str(path.as_wtf8());
        super::update_code_filenames(&code, old_name, new_name);
    }

    #[pyfunction]
    fn _frozen_module_names(vm: &VirtualMachine) -> Vec<PyObjectRef> {
        vm.state
            .frozen
            .keys()
            .map(|&name| vm.ctx.new_utf8_str(name).into())
            .collect()
    }

    #[derive(FromArgs)]
    struct FindFrozenArgs {
        #[pyarg(positional)]
        name: PyUtf8StrRef,
        #[pyarg(named, default = false)]
        withdata: bool,
    }

    #[allow(clippy::type_complexity)]
    #[pyfunction]
    fn find_frozen(
        args: FindFrozenArgs,
        vm: &VirtualMachine,
    ) -> PyResult<Option<(Option<PyRef<PyMemoryView>>, bool, Option<PyStrRef>)>> {
        let FindFrozenArgs { name, withdata } = args;

        let name_str = name.as_str();
        let info = match super::find_frozen(name_str, vm) {
            Ok(info) => info,
            Err(FrozenError::NotFound | FrozenError::Disabled | FrozenError::BadName) => {
                return Ok(None);
            }
            Err(e) => return Err(e.to_pyexception(name_str, vm)),
        };

        // The data is what get_frozen_object() takes back, i.e. marshalled code.
        // Frozen modules are stored in their own encoding, so it has to be
        // re-serialized rather than handed out as a view of the stored bytes.
        let data = if withdata {
            let code = PyCode::new_ref_from_frozen(vm, info.code);
            let dumps = vm.import("marshal", 0)?.get_attr("dumps", vm)?;
            let bytes = dumps.call((code,), vm)?;
            Some(PyMemoryView::from_object(&bytes, vm)?.into_ref(&vm.ctx))
        } else {
            None
        };

        // When origname is empty (e.g. __hello_only__), return None.
        // Otherwise return the resolved alias name.
        let origname_str = super::resolve_frozen_alias(name_str);
        let origname = if origname_str.is_empty() {
            None
        } else {
            Some(vm.ctx.new_utf8_str(origname_str).into())
        };
        Ok(Some((data, info.package, origname)))
    }

    #[pyfunction]
    fn source_hash(key: u64, source: PyBytesRef) -> Vec<u8> {
        let hash: u64 = crate::common::hash::keyed_hash(key, source.as_bytes());
        hash.to_le_bytes().to_vec()
    }
}

fn update_code_filenames(
    code: &PyCode,
    old_name: &'static PyStrInterned,
    new_name: &'static PyStrInterned,
) {
    let current = code.source_path();
    if !core::ptr::eq(current, old_name) && current.as_str() != old_name.as_str() {
        return;
    }
    code.set_source_path(new_name);
    for constant in code.code.constants.iter() {
        let obj: &crate::PyObject = constant.borrow();
        if let Some(inner_code) = obj.downcast_ref::<PyCode>() {
            update_code_filenames(inner_code, old_name, new_name);
        }
    }
}

// ---------------------------------------------------------------------------
// PEP 489 dynamic-extension helpers (shared with capi through crate::import).
// ---------------------------------------------------------------------------

fn module_name_of(module: &crate::Py<crate::builtins::PyModule>, vm: &VirtualMachine) -> String {
    module
        .dict()
        .get_item_opt(rustpython_vm::identifier!(vm, __name__), vm)
        .ok()
        .flatten()
        .and_then(|o| o.downcast_ref::<crate::builtins::PyStr>().map(|s| s.to_string()))
        .unwrap_or_else(|| "<unknown>".to_owned())
}

/// Equivalent of CPython's PyModule_FromDefAndSpec2 for the result of a
/// multi-phase init function (PEP 489).
#[cfg(any(unix, windows))]
fn build_multiphase_module(
    spec: crate::PyObjectRef,
    name: &crate::builtins::PyUtf8StrRef,
    def_ptr: *mut crate::PyObject,
    vm: &VirtualMachine,
) -> PyResult {
    let def_ptr_usize = def_ptr as usize;

    // PyModuleDef_Init(def): idempotent.
    ensure_extension_def_initialized(def_ptr_usize);
    let def = unsafe { &*(def_ptr as *const CPyModuleDef) };

    let name_str = name.as_str();
    if def.m_size < 0 {
        return Err(vm.new_system_error(format!(
            "module {name_str}: m_size may not be negative for multi-phase initialization"
        )));
    }

    // Scan the slot array.
    let mut create: Option<
        unsafe extern "C" fn(
            *mut crate::PyObject,
            *mut CPyModuleDef,
        ) -> *mut crate::PyObject,
    > = None;
    let mut has_execution_slots = false;
    let mut has_multiple_interpreters_slot = false;
    let mut has_gil_slot = false;
    if !def.m_slots.is_null() {
        let mut i = 0usize;
        loop {
            let slot = unsafe { &*def.m_slots.add(i) };
            if slot.slot == 0 {
                break;
            }
            match slot.slot {
                legacy_slot_ids::Py_mod_create => {
                    if create.is_some() {
                        return Err(vm.new_system_error(format!(
                            "module {name_str} has multiple create slots"
                        )));
                    }
                    create = Some(unsafe {
                        core::mem::transmute::<
                            *mut core::ffi::c_void,
                            unsafe extern "C" fn(
                                *mut crate::PyObject,
                                *mut CPyModuleDef,
                            ) -> *mut crate::PyObject,
                        >(slot.value)
                    });
                }
                legacy_slot_ids::Py_mod_exec => {
                    has_execution_slots = true;
                }
                legacy_slot_ids::Py_mod_multiple_interpreters => {
                    if has_multiple_interpreters_slot {
                        return Err(vm.new_system_error(format!(
                            "module {name_str} has more than one 'multiple interpreters' slots"
                        )));
                    }
                    has_multiple_interpreters_slot = true;
                }
                legacy_slot_ids::Py_mod_gil => {
                    if has_gil_slot {
                        return Err(vm.new_system_error(format!(
                            "module {name_str} has more than one 'gil' slots"
                        )));
                    }
                    has_gil_slot = true;
                }
                _ => {
                    return Err(vm.new_system_error(format!(
                        "module {name_str} uses unknown slot ID {}",
                        slot.slot
                    )));
                }
            }
            i += 1;
        }
    }

    // Py_mod_create slot, or a fresh module.
    let module: crate::PyObjectRef = if let Some(create) = create {
        let created = unsafe { create(spec.as_object().as_raw().cast_mut(), def_ptr.cast()) };
        if created.is_null() {
            return match vm.take_raised_exception() {
                Some(exc) => Err(exc),
                None => Err(vm.new_system_error(format!(
                    "creation of module {name_str} failed without setting an exception"
                ))),
            };
        }
        let obj = unsafe { (&*created).to_owned() };
        if let Some(exc) = vm.take_raised_exception() {
            return Err(system_error_from_cause(
                vm,
                format!("creation of module {name_str} raised unreported exception"),
                exc,
            ));
        }
        obj
    } else {
        vm.new_module(name_str, vm.ctx.new_dict(), None).into()
    };

    if module.downcast_ref::<crate::builtins::PyModule>().is_some() {
        set_extension_module_def(vm, &module, def_ptr_usize)?;
    } else {
        if def.m_size > 0 || def.m_traverse.is_some() || def.m_clear.is_some() || def.m_free.is_some() {
            return Err(vm.new_system_error(format!(
                "module {name_str} is not a module object, but requests module state"
            )));
        }
        if has_execution_slots {
            return Err(vm.new_system_error(format!(
                "module {name_str} specifies execution slots, but did not create a ModuleType instance"
            )));
        }
    }

    if !def.m_methods.is_null() {
        unsafe { add_c_methods_to_object(vm, &module, def.m_methods) }?;
    }
    if !def.m_doc.is_null() {
        let doc = unsafe { core::ffi::CStr::from_ptr(def.m_doc) }
            .to_str()
            .map_err(|_| vm.new_system_error("module docstring is not valid UTF-8"))?;
        module.set_attr(
            rustpython_vm::identifier!(vm, __doc__),
            vm.ctx.new_str(doc),
            vm,
        )?;
    }
    Ok(module)
}

/// Second and later loads of a single-phase extension (Python/import.c's
/// reload_singlephase_extension).
#[cfg(any(unix, windows))]
fn reload_singlephase_extension(
    entry: &SinglePhaseCacheEntry,
    name: &str,
    vm: &VirtualMachine,
) -> PyResult {
    let def = unsafe { &*(entry.def_ptr as *const CPyModuleDef) };
    if def.m_size == -1 {
        // The module does not support repeated initialization: reuse the
        // module from sys.modules if still there, otherwise create a new one
        // and copy the cached __dict__.
        let sys_modules = vm.sys_module.get_attr("modules", vm).unwrap();
        let sys_modules = sys_modules
            .downcast_ref::<crate::builtins::PyDict>()
            .ok_or_else(|| vm.new_system_error("sys.modules is not a dict"))?;
        if let Some(existing) = sys_modules.get_item_opt(name, vm)? {
            return Ok(existing);
        }
        let module: crate::PyObjectRef = vm.new_module(name, vm.ctx.new_dict(), None).into();
        if let Some(m_dict) = entry.m_dict {
            let cached = unsafe { &*(m_dict as *const crate::PyObject) }
                .downcast_ref::<crate::builtins::PyDict>()
                .ok_or_else(|| vm.new_system_error("invalid cached single-phase module dict"))?;
            let dict = module
                .downcast_ref::<crate::builtins::PyModule>()
                .ok_or_else(|| vm.new_system_error("module object is not a module"))?
                .dict();
            // The import bootstrap re-sets the import-managed attributes from
            // the spec; copying them would leak the previous loader/spec
            // (visible to the Source/Frozen importlib test variants).
            const SKIP: [&str; 5] = ["__name__", "__loader__", "__spec__", "__package__", "__file__"];
            for (k, v) in cached {
                let skip = match k.as_object().str_utf8(vm) {
                    Ok(k) => SKIP.contains(&k.as_str()),
                    Err(_) => false,
                };
                if !skip {
                    dict.set_item(k.as_object(), v, vm)?;
                }
            }
        }
        set_extension_module_def(vm, &module, entry.def_ptr)?;
        return Ok(module);
    }
    // m_size >= 0: re-run the init function.
    let init_fn: unsafe extern "C" fn() -> *mut crate::PyObject =
        unsafe { core::mem::transmute(entry.m_init) };
    let module_ptr = unsafe { init_fn() };
    if module_ptr.is_null() {
        return match vm.take_raised_exception() {
            Some(exc) => Err(exc),
            None => Err(vm.new_system_error(format!(
                "initialization of {name} failed without raising an exception"
            ))),
        };
    }
    if let Some(exc) = vm.take_raised_exception() {
        return Err(system_error_from_cause(
            vm,
            format!("initialization of {name} raised unreported exception"),
            exc,
        ));
    }
    Ok(unsafe { (&*module_ptr).to_owned() })
}

/// Register a successfully loaded single-phase module in the global cache
/// (Python/import.c's update_global_state_for_extension).
#[cfg(any(unix, windows))]
fn register_singlephase_cache(
    origin: &str,
    name: &str,
    module_ptr: *mut crate::PyObject,
    def_ptr: usize,
    init_fn: usize,
    vm: &VirtualMachine,
) {
    if def_ptr == 0 {
        return;
    }
    let def = unsafe { &*(def_ptr as *const CPyModuleDef) };
    let entry = if def.m_size == -1 {
        let dict: crate::PyObjectRef = match unsafe { &*module_ptr }
            .downcast_ref::<crate::builtins::PyModule>()
        {
            Some(m) => m.dict().into(),
            None => vm.ctx.none(),
        };
        // The cache stores a raw pointer; leak a clone so the dict stays
        // alive for the lifetime of the process (like CPython's m_copy).
        let raw = dict.as_object().as_raw() as usize;
        core::mem::forget(dict);
        SinglePhaseCacheEntry {
            def_ptr,
            m_dict: Some(raw),
            m_init: 0,
        }
    } else {
        SinglePhaseCacheEntry {
            def_ptr,
            m_dict: None,
            m_init: init_fn,
        }
    };
    extension_cache_put(origin, name, entry);
}
