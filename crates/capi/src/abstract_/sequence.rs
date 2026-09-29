use crate::{PyObject, pystate::with_vm};
use core::ffi::c_int;
use rustpython_vm::builtins::PyList;
use rustpython_vm::{AsObject, PyObjectRef, PyResult, VirtualMachine};

unsafe fn sequence_operand(
    vm: &VirtualMachine,
    raw: *mut PyObject,
) -> PyResult<PyObjectRef> {
    if unsafe { crate::listobject::is_foreign_list_buffer(raw) } {
        unsafe { crate::listobject::foreign_list_to_native(vm, raw) }
    } else {
        Ok(unsafe { &*raw }.to_owned())
    }
}

fn finish_foreign_inplace(
    vm: &VirtualMachine,
    raw: *mut PyObject,
    result: PyObjectRef,
) -> PyResult<*mut PyObject> {
    {
        let list = result.try_downcast_ref::<PyList>(vm)?;
        unsafe { crate::listobject::foreign_list_sync_from_native(vm, raw, list)? };
    }
    unsafe { crate::refcount::_Py_IncRef(raw) };
    Ok(raw)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Check(obj: *mut PyObject) -> c_int {
    if unsafe { crate::listobject::is_foreign_list_buffer(obj) } {
        return 1;
    }
    with_vm(|_vm| {
        let obj = unsafe { &*obj };
        Ok(obj.sequence_unchecked().check())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Concat(
    obj1: *mut PyObject,
    obj2: *mut PyObject,
) -> *mut PyObject {
    with_vm(|vm| {
        let obj1 = unsafe { sequence_operand(vm, obj1)? };
        let obj2 = unsafe { sequence_operand(vm, obj2)? };
        obj1.try_sequence(vm)?.concat(obj2.as_ref(), vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Count(obj: *mut PyObject, value: *mut PyObject) -> isize {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        let value = unsafe { &*value }.to_owned();
        obj.try_sequence(vm)?.count(value.as_ref(), vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_DelItem(obj: *mut PyObject, index: isize) -> c_int {
    let is_foreign = unsafe { crate::listobject::is_foreign_list_buffer(obj) };
    with_vm(|vm| {
        let object = unsafe { sequence_operand(vm, obj)? };
        if is_foreign {
            object.try_sequence(vm)?.del_item(index, vm)?;
            unsafe {
                crate::listobject::foreign_list_sync_from_native(
                    vm,
                    obj,
                    object.try_downcast_ref::<PyList>(vm)?,
                )?
            };
            Ok(())
        } else {
            object.try_sequence(vm)?.del_item(index, vm)
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_DelSlice(obj: *mut PyObject, low: isize, high: isize) -> c_int {
    let is_foreign = unsafe { crate::listobject::is_foreign_list_buffer(obj) };
    with_vm(|vm| {
        let object = unsafe { sequence_operand(vm, obj)? };
        if is_foreign {
            object.try_sequence(vm)?.del_slice(low, high, vm)?;
            unsafe {
                crate::listobject::foreign_list_sync_from_native(
                    vm,
                    obj,
                    object.try_downcast_ref::<PyList>(vm)?,
                )?
            };
            Ok(())
        } else {
            object.try_sequence(vm)?.del_slice(low, high, vm)
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_GetItem(obj: *mut PyObject, index: isize) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        obj.try_sequence(vm)?.get_item(index, vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_GetSlice(
    obj: *mut PyObject,
    low: isize,
    high: isize,
) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        obj.try_sequence(vm)?.get_slice(low, high, vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_InPlaceConcat(
    obj1: *mut PyObject,
    obj2: *mut PyObject,
) -> *mut PyObject {
    let is_foreign = unsafe { crate::listobject::is_foreign_list_buffer(obj1) };
    with_vm(|vm| {
        let object = unsafe { sequence_operand(vm, obj1)? };
        let other = unsafe { sequence_operand(vm, obj2)? };
        let result = object.try_sequence(vm)?.inplace_concat(other.as_ref(), vm)?;
        if is_foreign {
            finish_foreign_inplace(vm, obj1, result)
        } else {
            Ok(result.into_raw().as_ptr())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_InPlaceRepeat(
    obj: *mut PyObject,
    count: isize,
) -> *mut PyObject {
    let is_foreign = unsafe { crate::listobject::is_foreign_list_buffer(obj) };
    with_vm(|vm| {
        let object = unsafe { sequence_operand(vm, obj)? };
        let result = object.try_sequence(vm)?.inplace_repeat(count, vm)?;
        if is_foreign {
            finish_foreign_inplace(vm, obj, result)
        } else {
            Ok(result.into_raw().as_ptr())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Index(obj: *mut PyObject, value: *mut PyObject) -> isize {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        let value = unsafe { &*value }.to_owned();
        obj.try_sequence(vm)?.index(value.as_ref(), vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_List(obj: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        Ok(obj.try_sequence(vm)?.list(vm))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Repeat(obj: *mut PyObject, count: isize) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        obj.try_sequence(vm)?.repeat(count, vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_SetItem(
    obj: *mut PyObject,
    index: isize,
    value: *mut PyObject,
) -> c_int {
    let is_foreign = unsafe { crate::listobject::is_foreign_list_buffer(obj) };
    with_vm(|vm| {
        let object = unsafe { sequence_operand(vm, obj)? };
        let value = unsafe { &*value }.to_owned();
        if is_foreign {
            object.try_sequence(vm)?.set_item(index, value, vm)?;
            unsafe {
                crate::listobject::foreign_list_sync_from_native(
                    vm,
                    obj,
                    object.try_downcast_ref::<PyList>(vm)?,
                )?
            };
            Ok(())
        } else {
            object.try_sequence(vm)?.set_item(index, value, vm)
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_SetSlice(
    obj: *mut PyObject,
    low: isize,
    high: isize,
    value: *mut PyObject,
) -> c_int {
    let is_foreign = unsafe { crate::listobject::is_foreign_list_buffer(obj) };
    with_vm(|vm| {
        let object = unsafe { sequence_operand(vm, obj)? };
        let value = unsafe { sequence_operand(vm, value)? };
        if is_foreign {
            object.try_sequence(vm)?.set_slice(low, high, value, vm)?;
            unsafe {
                crate::listobject::foreign_list_sync_from_native(
                    vm,
                    obj,
                    object.try_downcast_ref::<PyList>(vm)?,
                )?
            };
            Ok(())
        } else {
            object.try_sequence(vm)?.set_slice(low, high, value, vm)
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Size(obj: *mut PyObject) -> isize {
    if unsafe { crate::listobject::is_foreign_list_buffer(obj) } {
        return unsafe { crate::listobject::header_obj_size(obj) };
    }
    let r = with_vm(|vm| {
        let obj = unsafe { &*obj };
        obj.try_sequence(vm)?.length(vm)
    });
    if std::env::var("RUSTPYTHON_TRACE").is_ok() && r < 0 {
        eprintln!("TRACE: PySequence_Size({:p}) = {}", obj, r);
    }
    r
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Length(obj: *mut PyObject) -> isize {
    unsafe { PySequence_Size(obj) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Tuple(obj: *mut PyObject) -> *mut PyObject {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        Ok(obj.try_sequence(vm)?.tuple(vm))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Contains(obj: *mut PyObject, value: *mut PyObject) -> c_int {
    with_vm(|vm| {
        let obj = unsafe { sequence_operand(vm, obj)? };
        let value = unsafe { &*value }.to_owned();
        obj.try_sequence(vm)?.contains(value.as_ref(), vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_In(obj: *mut PyObject, value: *mut PyObject) -> c_int {
    unsafe { PySequence_Contains(obj, value) }
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;
    use pyo3::types::{PyAnyMethods, PyDict, PyList, PySequence, PySequenceMethods, PyTuple};

    #[test]
    fn item_and_size_ops() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3]).unwrap();
            let seq = list.cast_into::<PySequence>().unwrap();

            assert_eq!(seq.len().unwrap(), 3);
            assert_eq!(seq.get_item(1).unwrap().extract::<i32>().unwrap(), 2);

            seq.set_item(1, 4).unwrap();
            assert_eq!(seq.get_item(1).unwrap().extract::<i32>().unwrap(), 4);

            seq.del_item(1).unwrap();
            assert_eq!(seq.get_item(1).unwrap().extract::<i32>().unwrap(), 3);
        });
    }

    #[test]
    fn slice_ops() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3, 4]).unwrap();
            let seq = list.cast_into::<PySequence>().unwrap();

            let sub = seq.get_slice(1, 3).unwrap();
            assert_eq!(sub.get_item(0).unwrap().extract::<i32>().unwrap(), 2);
            assert_eq!(sub.get_item(1).unwrap().extract::<i32>().unwrap(), 3);

            let repl = PyList::new(py, [8, 9]).unwrap();
            seq.set_slice(1, 3, &repl).unwrap();
            assert_eq!(seq.get_item(1).unwrap().extract::<i32>().unwrap(), 8);
            assert_eq!(seq.get_item(2).unwrap().extract::<i32>().unwrap(), 9);

            seq.del_slice(1, 3).unwrap();
            assert_eq!(seq.get_item(0).unwrap().extract::<i32>().unwrap(), 1);
            assert_eq!(seq.get_item(1).unwrap().extract::<i32>().unwrap(), 4);
        });
    }

    #[test]
    fn concat_repeat_and_inplace_ops() {
        Python::attach(|py| {
            let list = PyList::new(py, [1, 2]).unwrap();
            let seq = list.cast_into::<PySequence>().unwrap();
            let rhs = PyList::new(py, [3])
                .unwrap()
                .cast_into::<PySequence>()
                .unwrap();

            let concat = seq.concat(&rhs).unwrap();
            assert_eq!(concat.get_item(2).unwrap().extract::<i32>().unwrap(), 3);

            let repeat = seq.repeat(2).unwrap();
            assert_eq!(repeat.get_item(2).unwrap().extract::<i32>().unwrap(), 1);

            let iadd_rhs = PyList::new(py, [4])
                .unwrap()
                .cast_into::<PySequence>()
                .unwrap();
            seq.in_place_concat(&iadd_rhs).unwrap();
            assert_eq!(seq.get_item(2).unwrap().extract::<i32>().unwrap(), 4);

            seq.in_place_repeat(2).unwrap();
            assert_eq!(seq.get_item(5).unwrap().extract::<i32>().unwrap(), 4);
        });
    }

    #[test]
    fn count_index_contains_and_convert_ops() {
        Python::attach(|py| {
            let tuple = PyTuple::new(py, [1, 2, 1])
                .unwrap()
                .cast_into::<PySequence>()
                .unwrap();

            assert_eq!(tuple.count(1).unwrap(), 2);
            assert_eq!(tuple.index(2).unwrap(), 1);
            assert!(tuple.contains(1).unwrap());

            let as_list = tuple.to_list().unwrap().cast_into::<PySequence>().unwrap();
            assert_eq!(as_list.get_item(0).unwrap().extract::<i32>().unwrap(), 1);

            let as_tuple = as_list
                .to_tuple()
                .unwrap()
                .cast_into::<PySequence>()
                .unwrap();
            assert_eq!(as_tuple.get_item(2).unwrap().extract::<i32>().unwrap(), 1);
        });
    }

    #[test]
    fn contains_works_for_dict() {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("k", 1).unwrap();

            let any = dict.into_any();
            assert!(any.contains("k").unwrap());
            assert!(!any.contains("missing").unwrap());
        });
    }
}
