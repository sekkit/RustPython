use crate::PyObject;
use crate::object::define_py_check;
use crate::pystate::with_vm;
use core::ffi::c_int;
use core::ptr::NonNull;
use rustpython_vm::AsObject;
use rustpython_vm::PyObjectRef;
use rustpython_vm::builtins::PyList;
use rustpython_vm::sliceable::{SaturatedSlice, SliceableSequenceMutOp, SliceableSequenceOp};

define_py_check!(fn PyList_Check, types.list_type);
define_py_check!(exact fn PyList_CheckExact, types.list_type);

/// True when `raw` is a CPython-layout list buffer: a foreign raw buffer whose
/// ob_type (at offset 8) is our exported `PyList_Type` stub. Such buffers are
/// created when a C extension fills a list via the inlined `PyList_SET_ITEM`
/// macro (which writes `ob_item[i]` at offset 0x18) — the CSP layout wall.
///
/// These objects are *not* live RustPython lists and cannot be downcast;
/// they must be materialized into a native `PyList` at the C↔Python boundary,
/// or accessed through the foreign (raw-array) branch of the `PyList_*` API.
///
/// # Safety
/// `raw` must be a valid non-null pointer whose ob_type word at offset 8 is
/// readable.
#[inline]
pub(crate) unsafe fn is_foreign_list_buffer(raw: *mut PyObject) -> bool {
    if raw.is_null() {
        return false;
    }
    #[allow(static_mut_refs)]
    let list_stub = core::ptr::addr_of!(crate::objectstatics::PyList_Type) as usize;
    let ob_type = unsafe { *(raw as *const usize).add(1) };
    ob_type == list_stub
}

/// Materialize a CPython-layout list buffer into a native RustPython `PyList`.
///
/// Reads the CPython-compatible header of `raw`:
/// `ob_refcnt@0x00`, `ob_type@0x08`, `ob_size@0x10`, `ob_item@0x18`.
/// Each non-null element in the `ob_item` array is incref'd into the returned
/// native list; the raw buffer keeps its own references (freed separately by
/// its dealloc path). This is the "boundary materialization" strategy: it turns
/// a C-side `PyList_SET_ITEM`-filled raw list into a real Python list so that
/// `len`/indexing/iteration work without a full dual-path implementation.
///
/// # Safety
/// `raw` must be a valid CPython-layout list buffer (see
/// `is_foreign_list_buffer`); `vm` must be live.
pub(crate) unsafe fn foreign_list_to_native(
    vm: &rustpython_vm::VirtualMachine,
    raw: *mut PyObject,
) -> rustpython_vm::PyResult<PyObjectRef> {
    // ob_size at offset 0x10, ob_item at offset 0x18.
    let size = unsafe { *(raw as *const usize).add(2) } as isize;
    let items_ptr = unsafe { *(raw as *const usize).add(3) } as *const *mut PyObject;
    let mut items = Vec::with_capacity(size.max(0) as usize);
    for i in 0..size {
        let item = unsafe { *items_ptr.offset(i as isize) };
        if !item.is_null() {
            // New owned reference for the native list; the raw array keeps its own.
            items.push(unsafe { (&*item).to_owned() });
        }
    }
    Ok(vm.ctx.new_list(items).into())
}

#[unsafe(no_mangle)]
pub extern "C" fn PyList_New(size: isize) -> *mut PyObject {
    // Boundary materialization: return a CPython-layout raw buffer so the
    // inlined PyList_SET_ITEM macro (numpy & co.) can write ob_item@0x18
    // safely. The buffer is materialized into a native PyList when it crosses
    // back into Python (wrap_foreign_object), and the foreign branch of the
    // PyList_* API reads/writes the raw arrays directly.
    with_vm(|vm| new_foreign_list(vm, size))
}

/// CPython-layout PyListObject header, mirrors the offsets numpy's inlined
/// `PyList_SET_ITEM` writes through (ob_item at 0x18).
///
/// # Safety
/// `raw` must be a valid CPython-layout list buffer (see `PyList_NewForeign`).
pub(crate) unsafe fn header_obj_size(raw: *mut PyObject) -> isize {
    unsafe { *(raw as *const usize).add(2) as isize }
}

/// # Safety
/// `raw` must be a valid CPython-layout list buffer.
pub(crate) unsafe fn header_obj_item(raw: *mut PyObject) -> *mut *mut PyObject {
    unsafe { *(raw as *const usize).add(3) as *mut *mut PyObject }
}

/// CPython-layout PyListObject header, mirrors the offsets numpy's inlined
/// `PyList_SET_ITEM` writes through (ob_item at 0x18).
const CPYTHON_LIST_HEADER_BYTES: usize = 0x28; // refcnt/type/size/item/allocated

/// Allocate a CPython-layout list buffer: a foreign raw buffer whose header
/// matches PyListObject (ob_refcnt@0, ob_type@8, ob_size@0x10, ob_item@0x18).
///
/// This is the "boundary materialization" trigger: numpy (and other CPython
/// ABI extensions) fill such a buffer through the inlined `PyList_SET_ITEM`
/// macro, which writes `ob_item[i]` at 0x18 — safe on a CPython-layout
/// buffer, unsafe on a native RustPython list. The buffer is registered as
/// foreign so that crossing back to Python goes through `wrap_foreign_object`,
/// which materializes it into a native `PyList`.
fn new_foreign_list(
    vm: &rustpython_vm::VirtualMachine,
    size: isize,
) -> rustpython_vm::PyResult<*mut PyObject> {
    if size < 0 {
        return Err(vm.new_system_error("Negative size passed to PyList_New"));
    }
    let n = size as usize;
    unsafe {
        let header = libc::malloc(CPYTHON_LIST_HEADER_BYTES) as *mut usize;
        let items =
            libc::calloc(n.max(1), core::mem::size_of::<*mut PyObject>()) as *mut *mut PyObject;
        if header.is_null() || items.is_null() {
            libc::free(header as *mut libc::c_void);
            libc::free(items as *mut libc::c_void);
            return Err(vm.new_memory_error("PyList_New: allocation failed"));
        }
        #[allow(static_mut_refs)]
        let list_stub = core::ptr::addr_of!(crate::objectstatics::PyList_Type) as usize;
        *header = 1; // ob_refcnt
        *header.add(1) = list_stub; // ob_type
        *header.add(2) = n; // ob_size
        *header.add(3) = items as usize; // ob_item
        *header.add(4) = n; // allocated
        let ptr = header as *mut PyObject;
        crate::objimpl::register_foreign_object(ptr as *const u8);
        Ok(ptr)
    }
}

/// Explicitly create a foreign CPython-layout list buffer (test/tooling entry
/// point); the standard `PyList_New` already returns such buffers.
#[unsafe(no_mangle)]
pub extern "C" fn PyList_NewForeign(size: isize) -> *mut PyObject {
    with_vm(|vm| new_foreign_list(vm, size))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Size(obj: *mut PyObject) -> isize {
    // Foreign (CPython-layout) list: read ob_size at offset 0x10 directly.
    if unsafe { is_foreign_list_buffer(obj) } {
        return unsafe { header_obj_size(obj) };
    }
    with_vm(|vm| {
        let list = unsafe { &*obj }.try_downcast_ref::<PyList>(vm)?;
        Ok(list.__len__())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetItemRef(obj: *mut PyObject, index: isize) -> *mut PyObject {
    // Foreign (CPython-layout) list: read ob_item array at offset 0x18.
    if unsafe { is_foreign_list_buffer(obj) } {
        return with_vm(|vm| {
            let size = unsafe { header_obj_size(obj) };
            if index < 0 || index >= size {
                return Err(vm.new_index_error(format!("list index out of range: {index}")));
            }
            let items = unsafe { header_obj_item(obj) };
            let item = unsafe { *items.offset(index as isize) };
            if item.is_null() {
                return Err(vm.new_index_error(format!("list index out of range: {index}")));
            }
            Ok(unsafe { (&*item).to_owned().into_raw().as_ptr() })
        });
    }
    with_vm(|vm| {
        let list = unsafe { &*obj }.try_downcast_ref::<PyList>(vm)?;
        index
            .try_into()
            .ok()
            .and_then(|index: usize| list.borrow_vec().get(index).map(ToOwned::to_owned))
            .ok_or_else(|| vm.new_index_error(format!("list index out of range: {index}")))
    })
}

/// PyList_GetItem: return a *borrowed* reference to the item at `index`.
/// The caller must not decref the result, and must not use it after the
/// list is modified (same contract as CPython).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetItem(obj: *mut PyObject, index: isize) -> *mut PyObject {
    with_vm(|vm| {
        let list = unsafe { &*obj }.try_downcast_ref::<PyList>(vm)?;
        let index: usize = index
            .try_into()
            .ok()
            .ok_or_else(|| vm.new_index_error(format!("list index out of range: {index}")))?;
        let ptr = {
            let vec = list.borrow_vec();
            let item = vec
                .get(index)
                .ok_or_else(|| vm.new_index_error(format!("list index out of range: {index}")))?;
            // Borrowed reference: return the raw pointer without incref.
            // The list's vector storage outlives the read lock, so the
            // pointer stays valid until the list is mutated (CPython
            // contract for borrowed references).
            item.as_object().as_raw().cast_mut()
        };
        Ok(ptr)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_SetItem(
    list: *mut PyObject,
    index: isize,
    item: *mut PyObject,
) -> c_int {
    // Foreign (CPython-layout) list: write ob_item array at offset 0x18.
    if unsafe { is_foreign_list_buffer(list) } {
        return with_vm(|vm| {
            let size = unsafe { header_obj_size(list) };
            if index < 0 || index >= size {
                return Err(vm.new_index_error(format!("list index out of range: {index}")));
            }
            let items = unsafe { header_obj_item(list) };
            let slot = unsafe { items.offset(index as isize) };
            // CPython PyList_SetItem: the new item is a strong reference that
            // *steals* into the list; decref the old slot contents first.
            unsafe {
                let old = *slot;
                if !old.is_null() {
                    core::mem::drop(PyObjectRef::from_raw(NonNull::new_unchecked(old)));
                }
                *slot = item;
            }
            Ok(())
        });
    }
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        let item = unsafe { PyObjectRef::from_raw(NonNull::new_unchecked(item)) };
        let index_error =
            || vm.new_index_error(format!("list assignment index out of range: {index}"));
        if index < 0 {
            return Err(index_error());
        }

        let mut list_mut = list.borrow_vec_mut();
        match index - list_mut.len() as isize {
            ..0 => {
                list_mut[index as usize] = item;
                Ok(())
            }
            // This is somewhat a hack, we assume that we are populating a list right after PyList_New
            0 if list_mut.capacity() > index as usize => {
                list_mut.push(item);
                Ok(())
            }
            0.. => Err(index_error()),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Append(list: *mut PyObject, item: *mut PyObject) -> c_int {
    // Foreign (CPython-layout) list: append by growing the ob_item array.
    if unsafe { is_foreign_list_buffer(list) } {
        return with_vm(|vm| {
            let size = unsafe { header_obj_size(list) };
            let allocated = unsafe { *(list as *const usize).add(4) as isize };
            if size >= allocated {
                // Grow: doubling allocation, CPython-style.
                let new_cap = (allocated.max(1) * 2) as usize;
                let items = unsafe { header_obj_item(list) };
                let new_items = unsafe {
                    libc::realloc(
                        items as *mut libc::c_void,
                        new_cap * core::mem::size_of::<*mut PyObject>(),
                    ) as *mut *mut PyObject
                };
                if new_items.is_null() {
                    return Err(vm.new_memory_error("PyList_Append: realloc failed"));
                }
                unsafe { *(list as *mut usize).add(3) = new_items as usize };
                unsafe { *(list as *mut usize).add(4) = new_cap };
            }
            let items = unsafe { header_obj_item(list) };
            unsafe {
                *items.offset(size as isize) = item;
                *(list as *mut usize).add(2) = (size + 1) as usize; // ob_size++
            }
            Ok(())
        });
    }
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        let item = unsafe { &*item }.to_owned();
        list.borrow_vec_mut().push(item);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Insert(
    list: *mut PyObject,
    index: isize,
    item: *mut PyObject,
) -> c_int {
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        let item = unsafe { &*item }.to_owned();
        let mut vec = list.borrow_vec_mut();
        let index = if index < 0 {
            index + vec.len() as isize
        } else {
            index
        }
        .clamp(0, vec.len() as isize) as usize;
        vec.insert(index, item);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Reverse(list: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        list.borrow_vec_mut().reverse();
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_AsTuple(list: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        Ok(vm.ctx.new_tuple(list.borrow_vec().to_vec()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetSlice(
    list: *mut PyObject,
    low: isize,
    high: isize,
) -> *mut PyObject {
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        let vec = list.borrow_vec();
        let sliced = vec.getitem_by_slice(vm, SaturatedSlice::from_parts(low, high, 1))?;
        Ok(vm.ctx.new_list(sliced))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_SetSlice(
    list: *mut PyObject,
    low: isize,
    high: isize,
    itemlist: *mut PyObject,
) -> c_int {
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        let slice = SaturatedSlice::from_parts(low, high, 1);
        let mut vec = list.borrow_vec_mut();

        if itemlist.is_null() {
            vec.delitem_by_slice(vm, slice)?;
            return Ok(());
        }

        let items: Vec<PyObjectRef> = unsafe { &*itemlist }.try_to_value(vm)?;
        vec.setitem_by_slice(vm, slice, &items)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Sort(list: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let list = unsafe { &*list }.try_downcast_ref::<PyList>(vm)?;
        vm.call_method(list.as_object(), "sort", ())?;
        Ok(())
    })
}

// Anchor so the linker keeps PyList_GetItem in the export table.
#[used]
static PY_LIST_GETITEM_ANCHOR: unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject =
    PyList_GetItem;

#[cfg(test)]
mod tests {
    use pyo3::exceptions::PyIndexError;
    use pyo3::prelude::*;
    use pyo3::types::{PyList, PyListMethods};

    #[test]
    fn create_list() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3]).unwrap();
            assert_eq!(list.len(), 3);
            assert_eq!(list.get_item(0).unwrap().extract::<u32>().unwrap(), 1);
            assert_eq!(list.get_item(1).unwrap().extract::<u32>().unwrap(), 2);
            assert_eq!(list.get_item(2).unwrap().extract::<u32>().unwrap(), 3);
            assert!(list.get_item(3).is_err());
        })
    }

    #[test]
    fn replace_item_in_list() {
        Python::attach(|py| {
            let list = PyList::new(py, [1]).unwrap();
            assert_eq!(list.len(), 1);
            list.set_item(0, 2).unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list.get_item(0).unwrap().extract::<u32>().unwrap(), 2);
        })
    }

    #[test]
    fn set_item_out_of_range() {
        Python::attach(|py| {
            let list = PyList::empty(py);
            assert!(
                list.set_item(0, 1)
                    .unwrap_err()
                    .is_instance_of::<PyIndexError>(py)
            );
        })
    }

    #[test]
    fn list_append() {
        Python::attach(|py| {
            let list = PyList::empty(py);
            assert_eq!(list.len(), 0);
            list.append(1).unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list.get_item(0).unwrap().extract::<u32>().unwrap(), 1);
        })
    }

    #[test]
    fn list_insert() {
        Python::attach(|py| {
            let list = PyList::empty(py);
            assert_eq!(list.len(), 0);
            list.insert(0, 1).unwrap();
            assert_eq!(list.len(), 1);
            list.insert(2, 3).unwrap();
            assert_eq!(list.get_item(1).unwrap().extract::<u32>().unwrap(), 3);
        })
    }

    #[test]
    fn list_reverse() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3]).unwrap();
            list.reverse().unwrap();
            assert_eq!(list.get_item(0).unwrap().extract::<u32>().unwrap(), 3);
            assert_eq!(list.get_item(2).unwrap().extract::<u32>().unwrap(), 1);
        })
    }

    #[test]
    fn list_as_tuple() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3]).unwrap();
            let tuple = list.to_tuple();
            assert_eq!(tuple.len(), 3);
            assert_eq!(tuple.get_item(0).unwrap().extract::<u32>().unwrap(), 1);

            list.set_item(0, 9).unwrap();
            assert_eq!(tuple.get_item(0).unwrap().extract::<u32>().unwrap(), 1);
        })
    }

    #[test]
    fn list_get_slice() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3, 4]).unwrap();
            let slice = list.get_slice(1, 10);
            assert_eq!(slice.len(), 3);
            assert_eq!(slice.get_item(0).unwrap().extract::<u32>().unwrap(), 2);
            assert_eq!(slice.get_item(2).unwrap().extract::<u32>().unwrap(), 4);
        })
    }

    #[test]
    fn list_set_slice() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3, 4]).unwrap();
            let repl = PyList::new(py, [8, 9]).unwrap();
            list.set_slice(1, 3, repl.as_any()).unwrap();

            assert_eq!(list.len(), 4);
            assert_eq!(list.get_item(0).unwrap().extract::<u32>().unwrap(), 1);
            assert_eq!(list.get_item(1).unwrap().extract::<u32>().unwrap(), 8);
            assert_eq!(list.get_item(2).unwrap().extract::<u32>().unwrap(), 9);
            assert_eq!(list.get_item(3).unwrap().extract::<u32>().unwrap(), 4);
        })
    }

    #[test]
    fn list_sort() {
        Python::attach(|py| {
            let list = PyList::new(py, [3, 1, 2]).unwrap();
            list.sort().unwrap();
            assert_eq!(list.get_item(0).unwrap().extract::<u32>().unwrap(), 1);
            assert_eq!(list.get_item(1).unwrap().extract::<u32>().unwrap(), 2);
            assert_eq!(list.get_item(2).unwrap().extract::<u32>().unwrap(), 3);
        })
    }
}
