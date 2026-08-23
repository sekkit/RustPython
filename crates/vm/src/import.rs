//! Import mechanics

use crate::{
    AsObject, Py, PyObjectRef, PyPayload, PyRef, PyResult,
    builtins::{PyCode, PyStr, PyUtf8Str, PyUtf8StrRef, traceback::PyTraceback},
    exceptions::types::PyBaseException,
    scope::Scope,
    vm::{VirtualMachine, resolve_frozen_alias, thread},
};pub(crate) fn check_pyc_magic_number_bytes(buf: &[u8]) -> bool {
    buf.starts_with(&crate::version::PYC_MAGIC_NUMBER_BYTES)
}

pub(crate) fn init_importlib_base(vm: &mut VirtualMachine) -> PyResult<PyObjectRef> {
    flame_guard!("init importlib");

    // importlib_bootstrap needs these and it inlines checks to sys.modules before calling into
    // import machinery, so this should bring some speedup
    #[cfg(all(feature = "threading", not(target_os = "wasi")))]
    import_builtin(vm, "_thread")?;
    import_builtin(vm, "_warnings")?;
    import_builtin(vm, "_weakref")?;

    let importlib = thread::enter_vm(vm, || {
        let bootstrap = import_frozen(vm, "_frozen_importlib")?;
        let install = bootstrap.get_attr("_install", vm)?;
        let imp = import_builtin(vm, "_imp")?;
        install.call((vm.sys_module.clone(), imp), vm)?;
        Ok(bootstrap)
    })?;
    vm.import_func = importlib.get_attr(identifier!(vm, __import__), vm)?;
    vm.importlib = importlib.clone();
    Ok(importlib)
}

#[cfg(feature = "host_env")]
pub(crate) fn init_importlib_package(vm: &VirtualMachine, importlib: PyObjectRef) -> PyResult<()> {
    use crate::{TryFromObject, builtins::PyListRef};

    thread::enter_vm(vm, || {
        flame_guard!("install_external");

        // same deal as imports above
        import_builtin(vm, crate::stdlib::os::MODULE_NAME)?;
        #[cfg(windows)]
        import_builtin(vm, "winreg")?;
        import_builtin(vm, "_io")?;
        import_builtin(vm, "marshal")?;

        let install_external = importlib.get_attr("_install_external_importers", vm)?;
        install_external.call((), vm)?;
        let zipimport_res = (|| -> PyResult<()> {
            let zipimport = vm.import("zipimport", 0)?;
            let zipimporter = zipimport.get_attr("zipimporter", vm)?;
            let path_hooks = vm.sys_module.get_attr("path_hooks", vm)?;
            let path_hooks = PyListRef::try_from_object(vm, path_hooks)?;
            path_hooks.insert(0, zipimporter);
            Ok(())
        })();
        if zipimport_res.is_err() {
            warn!("couldn't init zipimport")
        }
        Ok(())
    })
}

pub fn make_frozen(vm: &VirtualMachine, name: &str) -> PyResult<PyRef<PyCode>> {
    let frozen = vm.state.frozen.get(name).ok_or_else(|| {
        vm.new_import_error(
            format!("No such frozen object named {name}"),
            vm.ctx.new_utf8_str(name),
        )
    })?;
    Ok(PyCode::new_ref_from_frozen(vm, frozen.code))
}

pub fn import_frozen(vm: &VirtualMachine, module_name: &str) -> PyResult {
    let frozen = vm.state.frozen.get(module_name).ok_or_else(|| {
        vm.new_import_error(
            format!("No such frozen object named {module_name}"),
            vm.ctx.new_utf8_str(module_name),
        )
    })?;
    let module = import_code_obj(
        vm,
        module_name,
        PyCode::new_ref_from_frozen(vm, frozen.code),
        false,
    )?;
    debug_assert!(module.get_attr(identifier!(vm, __name__), vm).is_ok());
    let origname = resolve_frozen_alias(module_name);
    module.set_attr("__origname__", vm.ctx.new_utf8_str(origname), vm)?;
    Ok(module)
}

pub fn import_builtin(vm: &VirtualMachine, module_name: &str) -> PyResult {
    let sys_modules = vm.sys_module.get_attr("modules", vm)?;

    // Check if already in sys.modules (handles recursive imports)
    if let Ok(module) = sys_modules.get_item(module_name, vm) {
        return Ok(module);
    }

    // Try multi-phase init first (preferred for modules that import other modules)
    if let Some(&def) = vm.state.module_defs.get(module_name) {
        // Phase 1: Create and initialize module
        let module = def.create_module(vm)?;

        // Add to sys.modules BEFORE exec (critical for circular import handling)
        sys_modules.set_item(module_name, module.clone().into(), vm)?;

        // Phase 2: Call exec slot (can safely import other modules now)
        // If exec fails, remove the partially-initialized module from sys.modules
        if let Err(e) = def.exec_module(vm, &module) {
            let _ = sys_modules.del_item(module_name, vm);
            return Err(e);
        }

        return Ok(module.into());
    }

    // Module not found in module_defs
    Err(vm.new_import_error(
        format!("Cannot import builtin module {module_name}"),
        vm.ctx.new_utf8_str(module_name),
    ))
}

#[cfg(feature = "rustpython-compiler")]
pub fn import_file(
    vm: &VirtualMachine,
    module_name: &str,
    file_path: &str,
    content: &str,
) -> PyResult {
    let code = vm
        .compile_with_opts(
            content,
            crate::compiler::Mode::Exec,
            file_path,
            vm.compile_opts(),
        )
        .map_err(|err| err.into_pyexception(vm, Some(content)))?;
    import_code_obj(vm, module_name, code, true)
}

#[cfg(feature = "rustpython-compiler")]
pub fn import_source(vm: &VirtualMachine, module_name: &str, content: &str) -> PyResult {
    let code = vm
        .compile_with_opts(
            content,
            crate::compiler::Mode::Exec,
            "<source>",
            vm.compile_opts(),
        )
        .map_err(|err| err.into_pyexception(vm, Some(content)))?;
    import_code_obj(vm, module_name, code, false)
}

/// If `__spec__._initializing` is true, wait for the module to finish
/// initializing by calling `_lock_unlock_module`.
fn import_ensure_initialized(
    module: &PyObjectRef,
    name: &str,
    vm: &VirtualMachine,
) -> PyResult<()> {
    let initializing = match vm.get_attribute_opt(module.clone(), vm.ctx.intern_str("__spec__"))? {
        Some(spec) => match vm.get_attribute_opt(spec, vm.ctx.intern_str("_initializing"))? {
            Some(v) => v.try_to_bool(vm)?,
            None => false,
        },
        None => false,
    };
    if initializing {
        let lock_unlock = vm.importlib.get_attr("_lock_unlock_module", vm)?;
        lock_unlock.call((vm.ctx.new_utf8_str(name),), vm)?;
    }
    Ok(())
}

pub fn import_code_obj(
    vm: &VirtualMachine,
    module_name: &str,
    code_obj: PyRef<PyCode>,
    set_file_attr: bool,
) -> PyResult {
    let attrs = vm.ctx.new_dict();
    attrs.set_item(
        identifier!(vm, __name__),
        vm.ctx.new_utf8_str(module_name).into(),
        vm,
    )?;
    if set_file_attr {
        attrs.set_item(
            identifier!(vm, __file__),
            code_obj.source_path().to_object(),
            vm,
        )?;
    }
    let module = vm.new_module(module_name, attrs.clone(), None);

    // Store module in cache to prevent infinite loop with mutual importing libs:
    let sys_modules = vm.sys_module.get_attr("modules", vm)?;
    sys_modules.set_item(module_name, module.clone().into(), vm)?;

    // Execute main code in module:
    let scope = Scope::with_builtins(None, attrs, vm);
    vm.run_code_obj(code_obj, scope)?;
    Ok(module.into())
}

fn remove_importlib_frames_inner(
    vm: &VirtualMachine,
    tb: Option<PyRef<PyTraceback>>,
    always_trim: bool,
) -> (Option<PyRef<PyTraceback>>, bool) {
    let traceback = if let Some(tb) = tb {
        tb
    } else {
        return (None, false);
    };

    let file_name = traceback.frame.iframe().code().source_path().as_str();

    let (inner_tb, mut now_in_importlib) =
        remove_importlib_frames_inner(vm, traceback.next.lock().clone(), always_trim);
    if file_name == "_frozen_importlib" || file_name == "_frozen_importlib_external" {
        if traceback.frame.iframe().code().obj_name.as_str() == "_call_with_frames_removed" {
            now_in_importlib = true;
        }
        if always_trim || now_in_importlib {
            return (inner_tb, now_in_importlib);
        }
    } else {
        now_in_importlib = false;
    }

    (
        Some(
            PyTraceback::new(
                inner_tb,
                traceback.frame.clone(),
                traceback.lasti,
                traceback.lineno,
            )
            .into_ref(&vm.ctx),
        ),
        now_in_importlib,
    )
}

// TODO: This function should do nothing on verbose mode.
// TODO: Fix this function after making PyTraceback.next mutable
pub fn remove_importlib_frames(vm: &VirtualMachine, exc: &Py<PyBaseException>) {
    if vm.state.config.settings.verbose != 0 {
        return;
    }

    let always_trim = exc.fast_isinstance(vm.ctx.exceptions.import_error);

    if let Some(tb) = exc.__traceback__() {
        let trimmed_tb = remove_importlib_frames_inner(vm, Some(tb), always_trim).0;
        exc.set_traceback_typed(trimmed_tb);
    }
}

/// Get origin path from a module spec, checking has_location first.
pub(crate) fn get_spec_file_origin(
    spec: Option<&PyObjectRef>,
    vm: &VirtualMachine,
) -> Option<String> {
    let spec = spec?;

    let has_location = spec
        .get_attr("has_location", vm)
        .ok()
        .and_then(|v| v.try_to_bool(vm).ok())
        .unwrap_or(false);
    if !has_location {
        return None;
    }
    spec.get_attr("origin", vm).ok().and_then(|origin| {
        if vm.is_none(&origin) {
            None
        } else {
            origin
                .downcast_ref::<PyStr>()
                .and_then(|s| s.to_str().map(|s| s.to_owned()))
        }
    })
}

/// Check if a module file possibly shadows another module of the same name.
/// Compares the module's directory with the original sys.path[0] (derived from sys.argv[0]).
pub(crate) fn is_possibly_shadowing_path(origin: &str, vm: &VirtualMachine) -> bool {
    use std::path::Path;

    if vm.state.config.settings.safe_path {
        return false;
    }

    let origin_path = Path::new(origin);
    let parent = match origin_path.parent() {
        Some(p) => p,
        None => return false,
    };
    // For packages (__init__.py), look one directory further up
    let root = if origin_path.file_name() == Some("__init__.py".as_ref()) {
        parent.parent().unwrap_or_else(|| Path::new(""))
    } else {
        parent
    };

    // Compute original sys.path[0] from sys.argv[0] (the script path).
    // See: config->sys_path_0, which is set once
    // at initialization and never changes even if sys.path is modified.
    let sys_path_0 = (|| -> Option<String> {
        let argv = vm.sys_module.get_attr("argv", vm).ok()?;
        let argv0 = argv.get_item(&0usize, vm).ok()?;
        let argv0_str = argv0.downcast_ref::<PyUtf8Str>()?;
        let s = argv0_str.as_str();

        // For -c and REPL, original sys.path[0] is ""
        if s == "-c" || s.is_empty() {
            return Some(String::new());
        }
        // For scripts, original sys.path[0] is dirname(argv[0])
        Some(
            Path::new(s)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_owned(),
        )
    })();

    let sys_path_0 = match sys_path_0 {
        Some(p) => p,
        None => return false,
    };

    let cmp_path = if sys_path_0.is_empty() {
        match crate::host_env::os::current_dir() {
            Ok(d) => d.to_string_lossy().to_string(),
            Err(_) => return false,
        }
    } else {
        sys_path_0
    };

    root.to_str() == Some(cmp_path.as_str())
}

/// Check if a module name is in sys.stdlib_module_names.
/// Takes the original __name__ object to preserve str subclass behavior.
/// Propagates errors (e.g. TypeError for unhashable str subclass).
pub(crate) fn is_stdlib_module_name(name: &PyObjectRef, vm: &VirtualMachine) -> PyResult<bool> {
    let stdlib_names = match vm.sys_module.get_attr("stdlib_module_names", vm) {
        Ok(names) => names,
        Err(_) => return Ok(false),
    };
    if !stdlib_names.class().fast_issubclass(vm.ctx.types.set_type)
        && !stdlib_names
            .class()
            .fast_issubclass(vm.ctx.types.frozenset_type)
    {
        return Ok(false);
    }
    let result = vm.call_method(&stdlib_names, "__contains__", (name.clone(),))?;
    result.try_to_bool(vm)
}

/// PyImport_ImportModuleLevelObject
pub(crate) fn import_module_level(
    name: &Py<PyStr>,
    globals: Option<PyObjectRef>,
    fromlist: Option<PyObjectRef>,
    level: i32,
    vm: &VirtualMachine,
) -> PyResult {
    if level < 0 {
        return Err(vm.new_value_error("level must be >= 0"));
    }

    let name_str = match name.to_str() {
        Some(s) => s,
        None => {
            // Name contains surrogates. Like CPython, try sys.modules
            // lookup with the Python string key directly.
            if level == 0 {
                let sys_modules = vm.sys_module.get_attr("modules", vm)?;
                return sys_modules.get_item(name, vm).map_err(|_| {
                    vm.new_import_error(format!("No module named '{name}'"), name.to_owned())
                });
            }
            return Err(vm.new_import_error(format!("No module named '{name}'"), name.to_owned()));
        }
    };

    // Resolve absolute name
    let abs_name = if level > 0 {
        // When globals is not provided (Rust None), raise KeyError
        // matching resolve_name() where globals==NULL
        if globals.is_none() {
            return Err(vm.new_key_error(vm.ctx.new_str("'__name__' not in globals").into()));
        }
        let globals_ref = globals.as_ref().unwrap();
        // When globals is Python None, treat like empty mapping
        let empty_dict_obj;
        let globals_ref = if vm.is_none(globals_ref) {
            empty_dict_obj = vm.ctx.new_dict().into();
            &empty_dict_obj
        } else {
            globals_ref
        };
        let package = calc_package(Some(globals_ref), vm)?;
        if package.is_empty() {
            return Err(vm.new_import_error(
                "attempted relative import with no known parent package",
                vm.ctx.new_utf8_str(""),
            ));
        }
        resolve_name(name_str, &package, level as usize, vm)?
    } else {
        if name_str.is_empty() {
            return Err(vm.new_value_error("Empty module name"));
        }
        name_str.to_owned()
    };

    // import_get_module + import_find_and_load
    let sys_modules = vm.sys_module.get_attr("modules", vm)?;
    let module = match sys_modules.get_item(&*abs_name, vm) {
        Ok(m) if !vm.is_none(&m) => {
            import_ensure_initialized(&m, &abs_name, vm)?;
            m
        }
        _ => {
            let find_and_load = vm.importlib.get_attr("_find_and_load", vm)?;
            let abs_name_obj = vm.ctx.new_utf8_str(&*abs_name);
            find_and_load.call((abs_name_obj, vm.import_func.clone()), vm)?
        }
    };

    // Handle fromlist
    let has_from = match fromlist.as_ref().filter(|fl| !vm.is_none(fl)) {
        Some(fl) => fl.clone().try_to_bool(vm)?,
        None => false,
    };

    if has_from {
        let fromlist = fromlist.unwrap();
        // Only call _handle_fromlist if the module looks like a package
        // (has __path__). Non-module objects without __name__/__path__ would
        // crash inside _handle_fromlist; IMPORT_FROM handles per-attribute
        // errors with proper ImportError conversion.
        let has_path = vm
            .get_attribute_opt(module.clone(), vm.ctx.intern_str("__path__"))?
            .is_some();
        if has_path {
            let handle_fromlist = vm.importlib.get_attr("_handle_fromlist", vm)?;
            handle_fromlist.call((module, fromlist, vm.import_func.clone()), vm)
        } else {
            Ok(module)
        }
    } else if level == 0 || !name_str.is_empty() {
        match name_str.find('.') {
            None => Ok(module),
            Some(dot) => {
                let to_return = if level == 0 {
                    name_str[..dot].to_owned()
                } else {
                    let cut_off = name_str.len() - dot;
                    abs_name[..abs_name.len() - cut_off].to_owned()
                };
                match sys_modules.get_item(&*to_return, vm) {
                    Ok(m) => Ok(m),
                    Err(_) if level == 0 => {
                        // For absolute imports (level 0), try importing the
                        // parent. Matches _bootstrap.__import__ behavior.
                        let find_and_load = vm.importlib.get_attr("_find_and_load", vm)?;
                        let to_return_obj = vm.ctx.new_utf8_str(&*to_return);
                        find_and_load.call((to_return_obj, vm.import_func.clone()), vm)
                    }
                    Err(_) => {
                        // For relative imports (level > 0), raise KeyError
                        let to_return_obj: PyObjectRef = vm
                            .ctx
                            .new_utf8_str(format!("'{to_return}' not in sys.modules as expected"))
                            .into();
                        Err(vm.new_key_error(to_return_obj))
                    }
                }
            }
        }
    } else {
        Ok(module)
    }
}

/// resolve_name in import.c - resolve relative import name
fn resolve_name(name: &str, package: &str, level: usize, vm: &VirtualMachine) -> PyResult<String> {
    // Python: bits = package.rsplit('.', level - 1)
    // Rust: rsplitn(level, '.') gives maxsplit=level-1
    let parts: Vec<&str> = package.rsplitn(level, '.').collect();
    if parts.len() < level {
        return Err(vm.new_import_error(
            "attempted relative import beyond top-level package",
            vm.ctx.new_utf8_str(name),
        ));
    }
    // rsplitn returns parts right-to-left, so last() is the leftmost (base)
    let base = parts.last().unwrap();
    if name.is_empty() {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}.{name}"))
    }
}

/// _calc___package__ - calculate package from globals for relative imports
fn calc_package(globals: Option<&PyObjectRef>, vm: &VirtualMachine) -> PyResult<String> {
    let globals = globals.ok_or_else(|| {
        vm.new_import_error(
            "attempted relative import with no known parent package",
            vm.ctx.new_utf8_str(""),
        )
    })?;

    let package = globals.get_item("__package__", vm).ok();
    let spec = globals.get_item("__spec__", vm).ok();

    if let Some(ref pkg) = package
        && !vm.is_none(pkg)
    {
        let pkg_str: PyUtf8StrRef = pkg
            .clone()
            .downcast()
            .map_err(|_| vm.new_type_error("package must be a string"))?;
        // Warn if __package__ != __spec__.parent
        if let Some(ref spec) = spec
            && !vm.is_none(spec)
            && let Ok(parent) = spec.get_attr("parent", vm)
            && !pkg_str.is(&parent)
            && pkg_str
                .as_object()
                .rich_compare_bool(&parent, crate::types::PyComparisonOp::Ne, vm)
                .unwrap_or(false)
        {
            let parent_repr = parent
                .repr_utf8(vm)
                .map(|s| s.as_str().to_owned())
                .unwrap_or_default();
            let msg = format!(
                "__package__ != __spec__.parent ('{}' != {})",
                pkg_str.as_str(),
                parent_repr
            );
            let warn = vm
                .import("_warnings", 0)
                .and_then(|w| w.get_attr("warn", vm));
            if let Ok(warn_fn) = warn {
                let _ = warn_fn.call(
                    (
                        vm.ctx.new_str(msg),
                        vm.ctx.exceptions.deprecation_warning.to_owned(),
                    ),
                    vm,
                );
            }
        }
        return Ok(pkg_str.as_str().to_owned());
    } else if let Some(ref spec) = spec
        && !vm.is_none(spec)
        && let Ok(parent) = spec.get_attr("parent", vm)
        && !vm.is_none(&parent)
    {
        let parent_str: PyUtf8StrRef = parent
            .downcast()
            .map_err(|_| vm.new_type_error("package set to non-string"))?;
        return Ok(parent_str.as_str().to_owned());
    }

    // Fall back to __name__ and __path__
    let warn = vm
        .import("_warnings", 0)
        .and_then(|w| w.get_attr("warn", vm));
    if let Ok(warn_fn) = warn {
        let _ = warn_fn.call(
            (
                vm.ctx.new_str("can't resolve package from __spec__ or __package__, falling back on __name__ and __path__"),
                vm.ctx.exceptions.import_warning.to_owned(),
            ),
            vm,
        );
    }

    let mod_name = globals.get_item("__name__", vm).map_err(|_| {
        vm.new_import_error(
            "attempted relative import with no known parent package",
            vm.ctx.new_utf8_str(""),
        )
    })?;
    let mod_name_str: PyUtf8StrRef = mod_name
        .downcast()
        .map_err(|_| vm.new_type_error("__name__ must be a string"))?;
    let mut package = mod_name_str.as_str().to_owned();
    // If not a package (no __path__), strip last component.
    // Uses rpartition('.')[0] semantics: returns empty string when no dot.
    if globals.get_item("__path__", vm).is_err() {
        package = match package.rfind('.') {
            Some(dot) => package[..dot].to_owned(),
            None => String::new(),
        };
    }
    Ok(package)
}

// ============================================================================
// Dynamic (C) extension module loading — PEP 489
//
// Shared with the capi crate: `_imp.create_dynamic` / `_imp.exec_dynamic`
// (crates/vm/src/stdlib/_imp.rs) drive the loading, and the capi crate's
// PyModule_Create2 / PyModuleDef_Init feed the registries below.
//
// The C-ABI structs mirror CPython's Include/cpython/moduleobject.h exactly
// (a duplicate of crates/capi/src/moduleobject.rs, which the capi crate uses
// as its public types; both must stay in sync with the C layout).
// ============================================================================

/// Size of the object header that C code can see (refcount, vtable, GC bits,
/// GC pointers, type pointer) — see `crate::object::PyInner`.
pub const PYOBJECT_HEADER_BYTES: usize = crate::object::SIZEOF_PYOBJECT_HEAD;

/// PyObject_HEAD is opaque here: extensions allocate the base with CPython's
/// header, and we only touch fields at CPython offsets.
#[repr(C)]
pub struct CPyModuleDefBase {
    pub ob_head: [usize; 2], // ob_refcnt, ob_type (PyObject_HEAD)
    pub m_init: Option<unsafe extern "C" fn() -> *mut crate::PyObject>,
    pub m_index: isize,
    pub m_copy: *mut crate::PyObject,
}

#[repr(C)]
pub struct CPyModuleDefSlot {
    pub slot: c_int,
    pub value: *mut c_void,
}

/// The four PyMethodDef calling conventions. `ml_meth` is stored as a raw
/// usize (the C struct field is a union of function pointers; on all
/// supported platforms function pointers and data pointers have the same
/// size, and CPython itself casts between them). The field matching `flags`
/// is transmuted to the proper function type at call time.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct CPyMethodDef {
    pub ml_name: *const c_char,
    pub ml_meth: usize,
    pub ml_flags: c_int,
    pub ml_doc: *const c_char,
}

#[repr(C)]
pub struct CPyModuleDef {
    pub m_base: CPyModuleDefBase,
    pub m_name: *const c_char,
    pub m_doc: *const c_char,
    pub m_size: isize,
    pub m_methods: *const CPyMethodDef,
    pub m_slots: *const CPyModuleDefSlot,
    pub m_traverse:
        Option<unsafe extern "C" fn(*mut crate::PyObject, *mut c_void, *mut c_void) -> c_int>,
    pub m_clear: Option<unsafe extern "C" fn(*mut crate::PyObject) -> c_int>,
    pub m_free: Option<unsafe extern "C" fn(*mut crate::PyObject)>,
}

/// Legacy (3.14 and earlier) module slot ids, as in Include/cpython/moduleobject.h.
/// (CPython 3.15 / PEP 793 renumbers these into the 84..110 range.)
#[allow(non_upper_case_globals)]
pub mod legacy_slot_ids {
    use core::ffi::c_int;

    pub const Py_mod_create: c_int = 1;
    pub const Py_mod_exec: c_int = 2;
    pub const Py_mod_multiple_interpreters: c_int = 3;
    pub const Py_mod_gil: c_int = 4;
    pub const Py_mod_LAST_SLOT: c_int = 4;
}

use core::ffi::{c_char, c_int, c_void};
use num_traits::ToPrimitive;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

/// PyModuleDef* values that have been through PyModuleDef_Init: the init
/// function of a multi-phase module returns one of these.
static EXTENSION_DEFS: LazyLock<Mutex<HashSet<usize>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Single-phase (legacy) extension cache, keyed by (origin, name) — mirrors
/// CPython's global extension cache in Python/import.c.
static EXTENSION_SINGLEPHASE_CACHE: LazyLock<Mutex<HashMap<(String, String), SinglePhaseCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Per-module module index counter for defs that reach us without an index
/// (CPython's _PyImport_GetNextModuleIndex).
static EXTENSION_MODULE_INDEX: core::sync::atomic::AtomicIsize =
    core::sync::atomic::AtomicIsize::new(0);

/// One entry of the single-phase cache.
#[derive(Clone)]
pub struct SinglePhaseCacheEntry {
    pub def_ptr: usize,
    /// For defs with m_size == -1: a raw pointer to the module's __dict__
    /// (CPython's def->m_base.m_copy). A leaked strong reference keeps the
    /// dict alive for the process lifetime; raw storage keeps the cache
    /// thread-safe (PyObjectRef is not Send).
    pub m_dict: Option<usize>,
    /// For defs with m_size >= 0: the init function, re-run on every load
    /// (CPython's cached m_init).
    pub m_init: usize,
}

pub fn register_extension_def(def_ptr: usize) {
    EXTENSION_DEFS.lock().unwrap().insert(def_ptr);
}

pub fn is_extension_def(def_ptr: usize) -> bool {
    EXTENSION_DEFS.lock().unwrap().contains(&def_ptr)
}

// The def pointer and the executed marker are stored in the module's own
// __dict__ (like CPython's md_def/md_state) instead of a global registry
// keyed by raw pointer: object addresses are reused after freeing, which
// would make a stale entry falsely apply to a new module.

/// Dict key holding the PyModuleDef* a module was created from.
pub const EXTENSION_DEF_DICT_KEY: &str = "__rustpython_extension_def__";
/// Dict key marking that the Py_mod_exec slot already ran (m_size >= 0 defs).
pub const EXTENSION_EXECUTED_DICT_KEY: &str = "__rustpython_extension_executed__";

pub fn set_extension_module_def(
    vm: &VirtualMachine,
    module: &PyObjectRef,
    def_ptr: usize,
) -> PyResult<()> {
    if def_ptr != 0 {
        let dict = module
            .downcast_ref::<crate::builtins::PyModule>()
            .ok_or_else(|| vm.new_system_error("extension module object is not a module"))?
            .dict();
        dict.set_item(
            EXTENSION_DEF_DICT_KEY,
            vm.ctx.new_int(def_ptr).into(),
            vm,
        )?;
    }
    Ok(())
}

pub fn extension_module_def(vm: &VirtualMachine, module: &PyObjectRef) -> Option<usize> {
    let dict = module.downcast_ref::<crate::builtins::PyModule>()?.dict();
    let value = dict.get_item_opt(EXTENSION_DEF_DICT_KEY, vm).ok()??;
    let int = value.downcast_ref::<crate::builtins::PyInt>()?;
    int.as_bigint().to_usize()
}

/// Returns true the first time a module is marked, false if it was already
/// marked (i.e. the exec slot must not run again).
pub fn mark_extension_module_executed(
    vm: &VirtualMachine,
    module: &PyObjectRef,
) -> PyResult<bool> {
    let dict = module
        .downcast_ref::<crate::builtins::PyModule>()
        .ok_or_else(|| vm.new_system_error("extension module object is not a module"))?
        .dict();
    let already = dict
        .get_item_opt(EXTENSION_EXECUTED_DICT_KEY, vm)?
        .is_some();
    if !already {
        dict.set_item(
            EXTENSION_EXECUTED_DICT_KEY,
            vm.ctx.new_int(1).into(),
            vm,
        )?;
    }
    Ok(!already)
}

pub fn extension_cache_get(origin: &str, name: &str) -> Option<SinglePhaseCacheEntry> {
    EXTENSION_SINGLEPHASE_CACHE
        .lock()
        .unwrap()
        .get(&(origin.to_owned(), name.to_owned()))
        .cloned()
}

pub fn extension_cache_put(origin: &str, name: &str, entry: SinglePhaseCacheEntry) {
    EXTENSION_SINGLEPHASE_CACHE
        .lock()
        .unwrap()
        .insert((origin.to_owned(), name.to_owned()), entry);
}

/// Assign a fresh module index to a def (PyModuleDef_Init semantics) and
/// register it. Idempotent.
pub fn ensure_extension_def_initialized(def_ptr: usize) {
    let def = unsafe { &mut *(def_ptr as *mut CPyModuleDef) };
    if def.m_base.m_index == -1 || def.m_base.m_index == 0 {
        def.m_base.m_index =
            EXTENSION_MODULE_INDEX.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    }
    register_extension_def(def_ptr);
}

// ---------------------------------------------------------------------------
// C method construction (ported from crates/capi/src/methodobject.rs so the
// vm can build method objects for module/type methods defined in C).
// ---------------------------------------------------------------------------

use crate::function::{FuncArgs, HeapMethodDef, PosArgs, PyMethodFlags};

fn ret_ptr_to_pyresult(vm: &VirtualMachine, ret_ptr: *mut crate::PyObject) -> PyResult {
    // C code returning `Py_None` yields an exported `_Py_NoneStruct` symbol
    // (a header copy), not the real None object. Translate it so identity
    // checks (NoneType.__eq__) work. Two addresses may be registered: the
    // exe's own stub and the relay's copy, whichever the caller resolved.
    if !ret_ptr.is_null()
        && NONE_STUB_ADDRS
            .iter()
            .any(|a| a.load(core::sync::atomic::Ordering::Relaxed) == ret_ptr as usize)
    {
        return Ok(vm.ctx.none());
    }
    match core::ptr::NonNull::new(ret_ptr) {
        Some(ret_ptr) => {
            // Foreign raw-buffer objects (from _PyObject_New) must NOT be
            // wrapped as PyInner — they have no RustPython payload, typ or
            // GcPrefix, and touching them through the native object model
            // crashes. Ask the capi crate to wrap them safely instead.
            let raw = ret_ptr.as_ptr();
            if crate::object::foreign_dispatch::is_foreign(raw) {
                return match crate::object::foreign_dispatch::wrap_foreign(raw) {
                    Some(wrapped) => {
                        Ok(unsafe { PyObjectRef::from_raw(core::ptr::NonNull::new_unchecked(wrapped)) })
                    }
                    None => Err(vm.new_system_error(
                        "C extension returned a foreign object that cannot be wrapped",
                    )),
                };
            }
            Ok(unsafe { PyObjectRef::from_raw(ret_ptr) })
        }
        None => Err(match vm.take_raised_exception() {
            Some(exc) => exc,
            None => vm.new_system_error("NULL result without error in PyObject_Call"),
        }),
    }
}

/// Addresses of the exported `_Py_NoneStruct` symbols (the exe's own stub and
/// the relay's copy), registered by the capi crate at init so
/// `ret_ptr_to_pyresult` can translate them to the real None.
static NONE_STUB_ADDRS: [core::sync::atomic::AtomicUsize; 2] = [
    core::sync::atomic::AtomicUsize::new(0),
    core::sync::atomic::AtomicUsize::new(0),
];

pub fn register_none_stub_addr(addr: usize) {
    let first = NONE_STUB_ADDRS[0].load(core::sync::atomic::Ordering::Relaxed);
    let slot = usize::from(first != 0);
    NONE_STUB_ADDRS[slot].store(addr, core::sync::atomic::Ordering::Relaxed);
}

fn take_self_arg(args: &mut FuncArgs, flags: PyMethodFlags) -> Option<PyObjectRef> {
    if flags.contains(PyMethodFlags::STATIC) {
        None
    } else {
        args.take_positional()
    }
}

unsafe fn call_c_function<A: Into<FuncArgs>>(
    vm: &VirtualMachine,
    method: usize,
    flags: PyMethodFlags,
    has_self: bool,
    args: Option<A>,
) -> PyResult {
    let f: unsafe extern "C" fn(*mut crate::PyObject, *mut crate::PyObject) -> *mut crate::PyObject =
        unsafe { core::mem::transmute(method) };
    let (slf, arg_tuple) = if let Some(mut args) = args.map(Into::into) {
        let slf = if has_self { take_self_arg(&mut args, flags) } else { None };
        let arg_tuple = vm.ctx.new_tuple(args.args);
        (slf, Some(arg_tuple))
    } else {
        (None, None)
    };

    let slf_ptr = slf
        .as_ref()
        .map(|obj| obj.as_object().as_raw().cast_mut())
        .unwrap_or_default();

    let arg_ptr = arg_tuple
        .as_ref()
        .map(|tuple| tuple.as_object().as_raw().cast_mut())
        .unwrap_or_default();

    let ret_ptr = unsafe { f(slf_ptr, arg_ptr) };
    ret_ptr_to_pyresult(vm, ret_ptr)
}

unsafe fn call_c_function_with_keywords(
    vm: &VirtualMachine,
    method: usize,
    flags: PyMethodFlags,
    has_self: bool,
    mut args: FuncArgs,
) -> PyResult {
    let f: unsafe extern "C" fn(
        *mut crate::PyObject,
        *mut crate::PyObject,
        *mut crate::PyObject,
    ) -> *mut crate::PyObject = unsafe { core::mem::transmute(method) };
    let slf = if has_self { take_self_arg(&mut args, flags) } else { None };
    let slf_ptr = slf
        .as_ref()
        .map(|obj| obj.as_object().as_raw().cast_mut())
        .unwrap_or_default();
    let arg_tuple = vm.ctx.new_tuple(args.args);
    let kwargs = vm.ctx.new_dict();
    for (k, v) in args.kwargs {
        kwargs.set_item(&*k, v, vm)?;
    }
    let ret_ptr = unsafe {
        f(
            slf_ptr,
            arg_tuple.as_object().as_raw().cast_mut(),
            kwargs.as_object().as_raw().cast_mut(),
        )
    };
    ret_ptr_to_pyresult(vm, ret_ptr)
}

unsafe fn call_c_fast_function_with_keywords(
    vm: &VirtualMachine,
    method: usize,
    flags: PyMethodFlags,
    has_self: bool,
    mut args: FuncArgs,
) -> PyResult {
    let f: unsafe extern "C" fn(
        *mut crate::PyObject,
        *const *mut crate::PyObject,
        isize,
        *mut crate::PyObject,
    ) -> *mut crate::PyObject = unsafe { core::mem::transmute(method) };
    let slf = if has_self { take_self_arg(&mut args, flags) } else { None };
    let slf_ptr = slf
        .as_ref()
        .map(|obj| obj.as_object().as_raw().cast_mut())
        .unwrap_or_default();
    let nargs = args.args.len();
    let mut fastcall_args = args.args;
    let kwnames_tuple = if !args.kwargs.is_empty() {
        let mut kwnames = Vec::with_capacity(args.kwargs.len());
        for (k, v) in args.kwargs {
            kwnames.push(vm.ctx.new_str(k).into());
            fastcall_args.push(v);
        }
        Some(vm.ctx.new_tuple(kwnames))
    } else {
        None
    };
    let kwnames_ptr = kwnames_tuple
        .as_ref()
        .map(|tuple| tuple.as_object().as_raw().cast_mut())
        .unwrap_or_default();
    // SAFETY: PyObjectRef is repr(transparent) over a pointer to PyObject, so a
    // Vec<PyObjectRef> has a layout-compatible contiguous backing buffer. The
    // vector is kept alive for the duration of the call.
    let fastcall_arg_ptrs = fastcall_args.as_ptr().cast::<*mut crate::PyObject>();
    let ret_ptr = unsafe { f(slf_ptr, fastcall_arg_ptrs, nargs as isize, kwnames_ptr) };
    ret_ptr_to_pyresult(vm, ret_ptr)
}

unsafe fn call_c_fast_function(
    vm: &VirtualMachine,
    method: usize,
    flags: PyMethodFlags,
    has_self: bool,
    args: PosArgs,
) -> PyResult {
    let f: unsafe extern "C" fn(
        *mut crate::PyObject,
        *const *mut crate::PyObject,
        isize,
    ) -> *mut crate::PyObject = unsafe { core::mem::transmute(method) };
    let mut args: FuncArgs = args.into();
    let slf = if has_self { take_self_arg(&mut args, flags) } else { None };
    let slf_ptr = slf
        .as_ref()
        .map(|obj| obj.as_object().as_raw().cast_mut())
        .unwrap_or_default();
    // SAFETY: PyObjectRef is repr(transparent) over a pointer to PyObject, so a
    // Vec<PyObjectRef> has a layout-compatible contiguous backing buffer. The
    // vector is kept alive for the duration of the call.
    let fastcall_arg_ptrs = args.args.as_mut_ptr().cast::<*mut crate::PyObject>();
    let ret_ptr = unsafe { f(slf_ptr, fastcall_arg_ptrs, args.args.len() as isize) };
    ret_ptr_to_pyresult(vm, ret_ptr)
}

/// Build a method object for a C `PyMethodDef` entry (CPython's
/// PyCFunction_NewEx). `has_self` must be true for type methods and for
/// module functions (which are bound to the module); pass false for unbound
/// attributes on non-module objects. Names and docs come from C strings that
/// outlive the interpreter, hence the `'static` bounds.
pub fn build_c_method_def(
    vm: &VirtualMachine,
    name: &'static str,
    method: usize,
    flags: PyMethodFlags,
    has_self: bool,
    doc: Option<&'static str>,
) -> PyResult<PyRef<HeapMethodDef>> {
    if flags.contains(PyMethodFlags::METHOD) {
        return Err(vm.new_system_error("METH_METHOD is not supported"));
    }

    let call_flags = flags
        & (PyMethodFlags::VARARGS
            | PyMethodFlags::KEYWORDS
            | PyMethodFlags::NOARGS
            | PyMethodFlags::O
            | PyMethodFlags::FASTCALL);
    let has_self = has_self && !flags.contains(PyMethodFlags::STATIC);

    if call_flags == PyMethodFlags::NOARGS {
        if has_self {
            let callable = move |zelf: PyObjectRef, vm: &VirtualMachine| unsafe {
                let f: unsafe extern "C" fn(
                    *mut crate::PyObject,
                    *mut crate::PyObject,
                ) -> *mut crate::PyObject = core::mem::transmute(method);
                let ret_ptr = f(zelf.as_raw().cast_mut(), core::ptr::null_mut());
                ret_ptr_to_pyresult(vm, ret_ptr)
            };
            Ok(vm.ctx.new_method_def(name, callable, flags, doc))
        } else {
            let callable = move |vm: &VirtualMachine| unsafe {
                let f: unsafe extern "C" fn(
                    *mut crate::PyObject,
                    *mut crate::PyObject,
                ) -> *mut crate::PyObject = core::mem::transmute(method);
                let ret_ptr = f(core::ptr::null_mut(), core::ptr::null_mut());
                ret_ptr_to_pyresult(vm, ret_ptr)
            };
            Ok(vm.ctx.new_method_def(name, callable, flags, doc))
        }
    } else if call_flags == (PyMethodFlags::VARARGS | PyMethodFlags::KEYWORDS) {
        let name_static: &'static str = Box::leak(name.to_owned().into_boxed_str());
        let callable = move |args: FuncArgs, vm: &VirtualMachine| unsafe {
            crate::object::foreign_dispatch::set_current_fn_name(
                name_static.as_ptr() as usize,
                name_static.len(),
            );
            call_c_function_with_keywords(vm, method, flags, has_self, args)
        };
        Ok(vm.ctx.new_method_def(name, callable, flags, doc))
    } else if call_flags == (PyMethodFlags::FASTCALL | PyMethodFlags::KEYWORDS) {
        let callable = move |args: FuncArgs, vm: &VirtualMachine| unsafe {
            call_c_fast_function_with_keywords(vm, method, flags, has_self, args)
        };
        Ok(vm.ctx.new_method_def(name, callable, flags, doc))
    } else if call_flags == PyMethodFlags::FASTCALL {
        let callable = move |args: PosArgs, vm: &VirtualMachine| unsafe {
            call_c_fast_function(vm, method, flags, has_self, args)
        };
        Ok(vm.ctx.new_method_def(name, callable, flags, doc))
    } else if call_flags == PyMethodFlags::O {
        let f: unsafe extern "C" fn(
            *mut crate::PyObject,
            *mut crate::PyObject,
        ) -> *mut crate::PyObject = unsafe { core::mem::transmute(method) };
        if has_self {
            let callable = move |zelf: PyObjectRef, arg: PyObjectRef, vm: &VirtualMachine| -> PyResult {
                let ret_ptr = unsafe { f(zelf.as_raw().cast_mut(), arg.as_raw().cast_mut()) };
                ret_ptr_to_pyresult(vm, ret_ptr)
            };
            Ok(vm.ctx.new_method_def(name, callable, flags, doc))
        } else {
            let callable = move |arg: PyObjectRef, vm: &VirtualMachine| -> PyResult {
                let ret_ptr = unsafe { f(core::ptr::null_mut(), arg.as_raw().cast_mut()) };
                ret_ptr_to_pyresult(vm, ret_ptr)
            };
            Ok(vm.ctx.new_method_def(name, callable, flags, doc))
        }
    } else if call_flags == PyMethodFlags::VARARGS {
        let callable = move |args: PosArgs, vm: &VirtualMachine| unsafe {
            call_c_function(vm, method, flags, has_self, Some(args))
        };
        Ok(vm.ctx.new_method_def(name, callable, flags, doc))
    } else {
        Err(vm.new_system_error(format!(
            "function {name} has unsupported or invalid calling-convention flags: {flags:?}"
        )))
    }
}

/// Iterate a NUL-terminated C `PyMethodDef` table and add each method to
/// `obj` (a module's dict, or setattr for non-module objects like the
/// SimpleNamespace returned by a Py_mod_create function).
///
/// # Safety
///
/// `methods` must point to a valid, NUL-terminated PyMethodDef table that
/// stays alive for the duration of the call.
pub unsafe fn add_c_methods_to_object(
    vm: &VirtualMachine,
    obj: &PyObjectRef,
    methods: *const CPyMethodDef,
) -> PyResult<()> {
    if methods.is_null() {
        return Ok(());
    }
    let is_module = obj.downcast_ref::<crate::builtins::PyModule>().is_some();
    let mut n = 0usize;
    loop {
        let md = unsafe { &*methods.add(n) };
        if md.ml_name.is_null() {
            return Ok(());
        }
        if n > 10_000 {
            return Err(vm.new_system_error("PyMethodDef table is not NUL-terminated"));
        }
        let name = unsafe { core::ffi::CStr::from_ptr(md.ml_name) }
            .to_str()
            .map_err(|_| vm.new_system_error("PyMethodDef name is not valid UTF-8"))?;
        let doc = if md.ml_doc.is_null() {
            None
        } else {
            Some(
                unsafe { core::ffi::CStr::from_ptr(md.ml_doc) }
                    .to_str()
                    .map_err(|_| vm.new_system_error("PyMethodDef doc is not valid UTF-8"))?,
            )
        };
        let flags = PyMethodFlags::from_bits(md.ml_flags as u32)
            .ok_or_else(|| vm.new_system_error("PyMethodDef contains unknown flags"))?;
        // Module functions are bound to the module as their `self`, exactly
        // like CPython's PyModule_AddFunctions (PyCFunction_NewEx with the
        // module as self). Other objects get unbound methods set as attrs.
        let bound_self = if is_module {
            Some(obj.clone())
        } else {
            None
        };
        let method = build_c_method_def(vm, name, md.ml_meth, flags, is_module, doc)?
            .build_function(vm, bound_self);
        if is_module {
            obj.downcast_ref::<crate::builtins::PyModule>()
                .ok_or_else(|| vm.new_system_error("module object is not a module"))?
                .dict()
                .set_item(name, method.into(), vm)?;
        } else {
            obj.set_attr(name, method, vm)?;
        }
        n += 1;
    }
}

/// Format a SystemError that chains `cause` as __cause__ (CPython's
/// _PyErr_FormatFromCause).
pub fn system_error_from_cause(
    vm: &VirtualMachine,
    message: String,
    cause: crate::builtins::PyBaseExceptionRef,
) -> crate::builtins::PyBaseExceptionRef {
    let exc = vm.new_system_error(message);
    exc.set___cause__(Some(cause));
    exc
}
