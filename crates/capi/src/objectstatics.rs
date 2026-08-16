//! C-visible data symbols that CPython defines as static structs.
//!
//! Extensions take the *address* of these symbols (`&_Py_NoneStruct`,
//! `&PyUnicode_Type`, ...). The vm's objects live on the heap, so the
//! exported memory holds a byte-for-byte copy of the object header, refreshed
//! lazily on first VM access. The copy's refcount state is marked leaked so
//! Rust reference counting on the copy can never free the static memory, and
//! the immortal bit (bit 31 of the state word) makes CPython's inline
//! Py_INCREF/Py_DECREF treat it as immortal (see crates/common/src/refcount.rs
//! for the state layout: [1:destructed][1:published][1:leaked][30:weak][31:strong]).
//!
//! Extensions do not see these exe-local stubs directly: python314.dll
//! forwards every symbol to rustpythonapi.dll (see
//! bench/make_python_dll_shims.ps1), whose data storage is a copy of this
//! module's stubs (and of the PyExc_* slots, see pyerrors.rs). The relay is
//! name-independent, so it works when the interpreter runs as rustpython.exe
//! or as a venv's python.exe copy. The relay's addresses are registered here
//! so the vm can translate C-visible pointers to them.

use crate::PyObject;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use rustpython_vm::{AsObject, VirtualMachine};
#[repr(C, align(8))]
pub struct ObjectHeaderCopy {
    words: [usize; 16],
}

#[unsafe(no_mangle)]
pub static mut _Py_NoneStruct: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 16] };
#[unsafe(no_mangle)]
pub static mut PyUnicode_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 16] };
#[unsafe(no_mangle)]
pub static mut PyLong_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 16] };
#[unsafe(no_mangle)]
pub static mut PyBool_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 16] };

static STATICS_REFRESHED: AtomicBool = AtomicBool::new(false);

/// Copy the real object headers into the exported stubs. Called once, lazily,
/// from `with_vm` (the first C-API call has a live VM to copy from).
pub(crate) fn ensure_object_statics(vm: &VirtualMachine) {
    if STATICS_REFRESHED.load(Ordering::Relaxed) {
        return;
    }
    let size = rustpython_vm::import::PYOBJECT_HEADER_BYTES;
    #[allow(static_mut_refs)]
    unsafe {
        copy_header(&mut _Py_NoneStruct, vm.ctx.none().as_object().as_raw(), size);
        copy_header(&mut PyUnicode_Type, vm.ctx.types.str_type.as_object().as_raw(), size);
        copy_header(&mut PyLong_Type, vm.ctx.types.int_type.as_object().as_raw(), size);
        copy_header(&mut PyBool_Type, vm.ctx.types.bool_type.as_object().as_raw(), size);
        // Tell the vm where the None stub lives so it can translate C-returned
        // `Py_None` pointers into the real None object.
        rustpython_vm::import::register_none_stub_addr(
            core::ptr::addr_of!(_Py_NoneStruct) as usize,
        );
    }
    #[cfg(windows)]
    unsafe {
        relay::ensure_relay(vm);
    }
    STATICS_REFRESHED.store(true, Ordering::Release);
}

unsafe fn copy_header(dst: &mut ObjectHeaderCopy, src: *const PyObject, size: usize) {
    unsafe {
        core::ptr::copy_nonoverlapping(
            src.cast::<u8>(),
            dst.words.as_mut_ptr().cast::<u8>(),
            size,
        );
        // Mark the copy leaked (which also sets the C-side immortal bit).
        let state = dst.words.as_mut_ptr().cast::<usize>();
        *state |= 1usize << (usize::BITS - 3);
    }
}

/// The exported type-stub symbols (this exe's own) plus, when the relay is
/// loaded, the relay's copies — the addresses extensions actually resolve
/// their data imports to. `StubKind` mirrors the ordering in
/// bench/make_python_dll_shims.ps1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StubKind {
    Str,
    Int,
    Bool,
}

/// True when `addr` is a C-visible header stub for the given type kind.
pub(crate) fn is_type_stub_addr(addr: usize, kind: StubKind) -> bool {
    #[allow(static_mut_refs)]
    let own = match kind {
        StubKind::Str => core::ptr::addr_of!(PyUnicode_Type) as usize,
        StubKind::Int => core::ptr::addr_of!(PyLong_Type) as usize,
        StubKind::Bool => core::ptr::addr_of!(PyBool_Type) as usize,
    };
    if addr == own {
        return true;
    }
    #[cfg(windows)]
    {
        let idx = match kind {
            StubKind::Str => 0,
            StubKind::Int => 1,
            StubKind::Bool => 2,
        };
        addr == relay::RELAY_TYPE_STUBS[idx].load(Ordering::Relaxed)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Name-independent C-API relay (rustpythonapi.dll): extensions resolve the
/// python314.dll shim's forwards to it, and it re-exports the running
/// executable's C API through jmp thunks and data copies. See
/// bench/make_python_dll_shims.ps1.
#[cfg(windows)]
pub(crate) mod relay {
    use super::*;

    #[allow(clippy::upper_case_acronyms)]
    type HMODULE = *mut core::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryA(lpFileName: *const u8) -> HMODULE;
        fn GetProcAddress(hModule: HMODULE, lpProcName: *const u8) -> *mut core::ffi::c_void;
        fn GetModuleHandleW(lpModuleName: *const u16) -> HMODULE;
    }

    /// Address of the relay's copy of each header stub, in StubKind order.
    pub(crate) static RELAY_TYPE_STUBS: [AtomicUsize; 3] = [
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    ];

    static RELAY_INITED: AtomicBool = AtomicBool::new(false);

    /// Load rustpythonapi.dll (found next to the executable) and initialize
    /// it with the process image handle: its function thunks then jump to
    /// this executable's exports and its data storage mirrors this module's
    /// stubs and the PyExc_* slots. Runs after the exe's own stubs are
    /// filled, so the relay copies the final contents; the vm must translate
    /// the relay's data addresses, so register them here.
    ///
    /// Safety: only called from ensure_object_statics, once, with a live VM.
    pub(crate) unsafe fn ensure_relay(_vm: &VirtualMachine) {
        if RELAY_INITED.load(Ordering::Relaxed) {
            return;
        }
        let dll = b"rustpythonapi.dll\0";
        let relay = unsafe { LoadLibraryA(dll.as_ptr().cast()) };
        if relay.is_null() {
            // No relay beside the exe: extension loading will fail on its own.
            return;
        }
        let init_name = b"rustpythonapi_init\0";
        let init = unsafe { GetProcAddress(relay, init_name.as_ptr().cast()) };
        if !init.is_null() {
            let init_fn: unsafe extern "C" fn(*mut core::ffi::c_void) -> core::ffi::c_int =
                unsafe { core::mem::transmute(init) };
            let exe = unsafe { GetModuleHandleW(core::ptr::null()) };
            unsafe { init_fn(exe) };
        }
        unsafe {
            let get = |name: &[u8]| GetProcAddress(relay, name.as_ptr().cast()) as usize;
            rustpython_vm::import::register_none_stub_addr(get(b"_Py_NoneStruct\0"));
            RELAY_TYPE_STUBS[0].store(get(b"PyUnicode_Type\0"), Ordering::Release);
            RELAY_TYPE_STUBS[1].store(get(b"PyLong_Type\0"), Ordering::Release);
            RELAY_TYPE_STUBS[2].store(get(b"PyBool_Type\0"), Ordering::Release);
        }
        RELAY_INITED.store(true, Ordering::Release);
    }
}
