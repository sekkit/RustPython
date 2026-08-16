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

use crate::PyObject;
use core::sync::atomic::{AtomicBool, Ordering};
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
