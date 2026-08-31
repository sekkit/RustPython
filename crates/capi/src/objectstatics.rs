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
use core::mem::offset_of;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use rustpython_vm::builtins::PyType;
use rustpython_vm::types::PyTypeSlots;
use rustpython_vm::{AsObject, VirtualMachine};

// Offsets within PyInner<PyType> for CPython-compatible PyTypeObject fields.
// PyInner header is now 16 bytes (ref_count + typ) matching CPython's PyObject,
// so the payload starts at SIZEOF_PYOBJECT_HEAD.
const SLOTS_BASE: usize =
    rustpython_vm::object::SIZEOF_PYOBJECT_HEAD + offset_of!(PyType, slots);
const OFFSET_HASH: usize = SLOTS_BASE + offset_of!(PyTypeSlots, hash);
const OFFSET_CALL: usize = SLOTS_BASE + offset_of!(PyTypeSlots, call);
const OFFSET_STR: usize = SLOTS_BASE + offset_of!(PyTypeSlots, str);
const OFFSET_REPR: usize = SLOTS_BASE + offset_of!(PyTypeSlots, repr);
const OFFSET_GETATTRO: usize = SLOTS_BASE + offset_of!(PyTypeSlots, getattro);
const OFFSET_SETATTRO: usize = SLOTS_BASE + offset_of!(PyTypeSlots, setattro);
#[repr(C, align(8))]
pub struct ObjectHeaderCopy {
    words: [usize; 32],  // 256 bytes â€” covers tp_flags at offset 168
}

#[unsafe(no_mangle)]
pub static mut _Py_NoneStruct: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyUnicode_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyLong_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyBool_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut _Py_FalseStruct: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut _Py_TrueStruct: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut _Py_EllipsisObject: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyFloat_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PySlice_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyType_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
// ---------- numpy/PyTorch-needed type stubs ----------
#[unsafe(no_mangle)]
pub static mut PyBaseObject_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyBytes_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyCapsule_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyCFunction_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyComplex_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyDict_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyDictProxy_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyFrozenSet_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyGetSetDescr_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyList_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyMemberDescr_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyMemoryView_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyMethodDescr_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PySet_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut PyTuple_Type: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
// ---------- data object stubs ----------
#[unsafe(no_mangle)]
pub static mut _Py_NotImplementedStruct: ObjectHeaderCopy = ObjectHeaderCopy { words: [0; 32] };
#[unsafe(no_mangle)]
pub static mut _Py_ascii_whitespace: [u8; 128] = [0; 128];

static STATICS_REFRESHED: AtomicBool = AtomicBool::new(false);

/// Copy the real object headers into the exported stubs. Called once, lazily,
/// from `with_vm` (the first C-API call has a live VM to copy from).
pub(crate) fn ensure_object_statics(vm: &VirtualMachine) {
    if STATICS_REFRESHED.load(Ordering::Relaxed) {
        return;
    }
    // Sentinel placed in .data among the stubs; any startup writer that
    // overflows the stub region smashes these words and trips the check.
    #[allow(static_mut_refs)]
    const CANARY_WORD: u64 = 0xDEAD_BEEF_1234_5678;
    #[allow(static_mut_refs)]
    static mut STUB_REGION_CANARY: [u64; 8] = [CANARY_WORD; 8];
    fn verify_stub_region() {
        #[allow(static_mut_refs)]
        unsafe {
            let mut bad = false;
            if STUB_REGION_CANARY != [CANARY_WORD; 8] {
                eprintln!("CANARY: STUB_REGION_CANARY smashed! {:?}", STUB_REGION_CANARY);
                bad = true;
            }
            for (s, name) in [
                (&raw const PyUnicode_Type, "PyUnicode_Type"),
                (&raw const PyLong_Type, "PyLong_Type"),
                (&raw const PyBool_Type, "PyBool_Type"),
                (&raw const PyFloat_Type, "PyFloat_Type"),
                (&raw const PyDict_Type, "PyDict_Type"),
                (&raw const PyList_Type, "PyList_Type"),
                (&raw const PyTuple_Type, "PyTuple_Type"),
                (&raw const PyBytes_Type, "PyBytes_Type"),
            ] {
                let s = &*s;
                for i in 22..32 {
                    if s.words[i] != 0 {
                        eprintln!(
                            "CANARY: stub {} tail word {} = {:#x}",
                            name, i, s.words[i]
                        );
                        bad = true;
                    }
                }
            }
            if bad {
                eprintln!("CANARY: static-region overflow CONFIRMED");
            }
        }
    }
    let size = rustpython_vm::import::PYOBJECT_HEADER_BYTES;
    #[allow(static_mut_refs)]
    unsafe {
        copy_header(
            &mut _Py_NoneStruct,
            vm.ctx.none().as_object().as_raw(),
            size,
        );
        copy_header(
            &mut PyUnicode_Type,
            vm.ctx.types.str_type.as_object().as_raw(),
            size,
        );
        fill_type_stub(&mut PyUnicode_Type, vm.ctx.types.str_type.as_object().as_raw());
        copy_header(
            &mut PyLong_Type,
            vm.ctx.types.int_type.as_object().as_raw(),
            size,
        );
        fill_type_stub(&mut PyLong_Type, vm.ctx.types.int_type.as_object().as_raw());
        copy_header(
            &mut PyBool_Type,
            vm.ctx.types.bool_type.as_object().as_raw(),
            size,
        );
        fill_type_stub(&mut PyBool_Type, vm.ctx.types.bool_type.as_object().as_raw());
        copy_header(
            &mut _Py_FalseStruct,
            vm.ctx.false_value.as_object().as_raw(),
            size,
        );
        copy_header(
            &mut _Py_TrueStruct,
            vm.ctx.true_value.as_object().as_raw(),
            size,
        );
        copy_header(
            &mut _Py_EllipsisObject,
            vm.ctx.ellipsis.as_object().as_raw(),
            size,
        );
        copy_header(
            &mut PyFloat_Type,
            vm.ctx.types.float_type.as_object().as_raw(),
            size,
        );
        fill_type_stub(&mut PyFloat_Type, vm.ctx.types.float_type.as_object().as_raw());
        copy_header(
            &mut PySlice_Type,
            vm.ctx.types.slice_type.as_object().as_raw(),
            size,
        );
        fill_type_stub(&mut PySlice_Type, vm.ctx.types.slice_type.as_object().as_raw());
        copy_header(
            &mut PyType_Type,
            vm.ctx.types.type_type.as_object().as_raw(),
            size,
        );
        fill_type_stub(&mut PyType_Type, vm.ctx.types.type_type.as_object().as_raw());
        // ---------- numpy/PyTorch-needed type stubs ----------
        copy_header(&mut PyBaseObject_Type, vm.ctx.types.object_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyBaseObject_Type, vm.ctx.types.object_type.as_object().as_raw());
        copy_header(&mut PyBytes_Type, vm.ctx.types.bytes_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyBytes_Type, vm.ctx.types.bytes_type.as_object().as_raw());
        copy_header(&mut PyCapsule_Type, vm.ctx.types.capsule_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyCapsule_Type, vm.ctx.types.capsule_type.as_object().as_raw());
        copy_header(&mut PyCFunction_Type, vm.ctx.types.builtin_function_or_method_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyCFunction_Type, vm.ctx.types.builtin_function_or_method_type.as_object().as_raw());
        copy_header(&mut PyComplex_Type, vm.ctx.types.complex_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyComplex_Type, vm.ctx.types.complex_type.as_object().as_raw());
        copy_header(&mut PyDict_Type, vm.ctx.types.dict_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyDict_Type, vm.ctx.types.dict_type.as_object().as_raw());
        copy_header(&mut PyDictProxy_Type, vm.ctx.types.mappingproxy_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyDictProxy_Type, vm.ctx.types.mappingproxy_type.as_object().as_raw());
        copy_header(&mut PyFrozenSet_Type, vm.ctx.types.frozenset_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyFrozenSet_Type, vm.ctx.types.frozenset_type.as_object().as_raw());
        copy_header(&mut PyGetSetDescr_Type, vm.ctx.types.getset_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyGetSetDescr_Type, vm.ctx.types.getset_type.as_object().as_raw());
        copy_header(&mut PyList_Type, vm.ctx.types.list_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyList_Type, vm.ctx.types.list_type.as_object().as_raw());
        copy_header(&mut PyMemberDescr_Type, vm.ctx.types.member_descriptor_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyMemberDescr_Type, vm.ctx.types.member_descriptor_type.as_object().as_raw());
        copy_header(&mut PyMemoryView_Type, vm.ctx.types.memoryview_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyMemoryView_Type, vm.ctx.types.memoryview_type.as_object().as_raw());
        copy_header(&mut PyMethodDescr_Type, vm.ctx.types.method_descriptor_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyMethodDescr_Type, vm.ctx.types.method_descriptor_type.as_object().as_raw());
        copy_header(&mut PySet_Type, vm.ctx.types.set_type.as_object().as_raw(), size);
        fill_type_stub(&mut PySet_Type, vm.ctx.types.set_type.as_object().as_raw());
        copy_header(&mut PyTuple_Type, vm.ctx.types.tuple_type.as_object().as_raw(), size);
        fill_type_stub(&mut PyTuple_Type, vm.ctx.types.tuple_type.as_object().as_raw());
        copy_header(&mut _Py_NotImplementedStruct, vm.ctx.not_implemented.as_object().as_raw(), size);
        // _Py_ascii_whitespace: CPython's " \t\n\r\x0b\x0c" (7 bytes)
        _Py_ascii_whitespace = [
            0x20, 0x09, 0x0a, 0x0d, 0x0b, 0x0c, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        // Tell the vm where the None stub lives so it can translate C-returned
        // `Py_None` pointers into the real None object.
        rustpython_vm::import::register_none_stub_addr(core::ptr::addr_of!(_Py_NoneStruct) as usize);
    }
    #[cfg(windows)]
    unsafe {
        relay::ensure_relay(vm);
    }
    verify_stub_region();
    STATICS_REFRESHED.store(true, Ordering::Release);
}

unsafe fn copy_header(dst: &mut ObjectHeaderCopy, src: *const PyObject, size: usize) {
    unsafe {
        core::ptr::copy_nonoverlapping(src.cast::<u8>(), dst.words.as_mut_ptr().cast::<u8>(), size);
        // Mark the copy leaked (which also sets the C-side immortal bit).
        let state = dst.words.as_mut_ptr().cast::<usize>();
        *state |= 1usize << (usize::BITS - 3);
    }
}

/// After copying the object header, fill in CPython-compatible PyTypeObject
/// fields at the correct offsets so C extensions can read tp_name, tp_basicsize,
/// tp_itemsize directly from the exported type stubs.
///
/// CPython PyTypeObject offsets (64-bit, non-GIL-disabled):
///   24: tp_name (const char*)
///   32: tp_basicsize (Py_ssize_t)
///   40: tp_itemsize (Py_ssize_t)
///
/// RustPython layout of PyInner<PyType>:
///   SIZEOF_PYOBJECT_HEAD (16): PyType { base(8), bases(8), mro(8), subclasses(8), attributes(8), slots(40+) }
///   then PyType.slots (SLOTS_BASE) holds name, basicsize, itemsize, flags in order.
unsafe fn fill_type_stub(dst: &mut ObjectHeaderCopy, src: *const PyObject) {
    // `name` is pub(crate) in PyTypeSlots, so derive its offset from the
    // immediately following field (basicsize) minus the size of a &'static str.
    let name_offset = offset_of!(PyTypeSlots, basicsize) - core::mem::size_of::<&'static str>();
    // Read basic fields
    let name_ptr = unsafe { *(src.add(SLOTS_BASE + name_offset) as *const *const u8) };
    let basicsize = unsafe { *(src.add(SLOTS_BASE + offset_of!(PyTypeSlots, basicsize)) as *const usize) };
    let itemsize = unsafe { *(src.add(SLOTS_BASE + offset_of!(PyTypeSlots, itemsize)) as *const usize) };
    let flags = unsafe { *(src.add(SLOTS_BASE + offset_of!(PyTypeSlots, flags)) as *const u64) };
    // Read function pointer slots (AtomicCell<Option<fn>> stored as usize)
    let hash_fn = unsafe { *(src.add(OFFSET_HASH) as *const usize) };
    let call_fn = unsafe { *(src.add(OFFSET_CALL) as *const usize) };
    let str_fn = unsafe { *(src.add(OFFSET_STR) as *const usize) };
    let repr_fn = unsafe { *(src.add(OFFSET_REPR) as *const usize) };
    let getattro_fn = unsafe { *(src.add(OFFSET_GETATTRO) as *const usize) };
    let setattro_fn = unsafe { *(src.add(OFFSET_SETATTRO) as *const usize) };
    let words = &mut dst.words;
    words[3] = name_ptr as usize;    // tp_name at offset 24
    words[4] = basicsize;            // tp_basicsize at offset 32
    words[5] = itemsize;             // tp_itemsize at offset 40
    words[15] = hash_fn;             // tp_hash at offset 120
    words[16] = call_fn;             // tp_call at offset 128
    words[17] = str_fn;              // tp_str at offset 136
    words[18] = repr_fn;             // tp_repr at offset 144
    words[19] = getattro_fn;         // tp_getattro at offset 152
    words[20] = setattro_fn;         // tp_setattro at offset 160
    words[21] = flags as usize;      // tp_flags at offset 168
}

/// The exported type-stub symbols (this exe's own) plus, when the relay is
/// loaded, the relay's copies â€” the addresses extensions actually resolve
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
