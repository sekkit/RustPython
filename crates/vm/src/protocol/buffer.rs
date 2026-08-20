//! Buffer protocol
//! <https://docs.python.org/3/c-api/buffer.html>

use crate::{
    AsObject, Py, PyObject, PyObjectRef, PyPayload, PyRef, PyResult, TryFromBorrowedObject,
    VirtualMachine,
    common::{
        borrow::{BorrowedValue, BorrowedValueMut},
        lock::{MapImmutable, PyMutex, PyMutexGuard},
    },
    object::PyObjectPayload,
    sliceable::SequenceIndexOp,
};
use alloc::borrow::Cow;
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    fmt::Debug,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};
use itertools::Itertools;

pub struct BufferMethods {
    pub obj_bytes: fn(&PyBuffer) -> BorrowedValue<'_, [u8]>,
    pub obj_bytes_mut: fn(&PyBuffer) -> BorrowedValueMut<'_, [u8]>,
    pub release: fn(&PyBuffer),
    pub retain: fn(&PyBuffer),
}

impl Debug for BufferMethods {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BufferMethods")
            .field("obj_bytes", &(self.obj_bytes as usize))
            .field("obj_bytes_mut", &(self.obj_bytes_mut as usize))
            .field("release", &(self.release as usize))
            .field("retain", &(self.retain as usize))
            .finish()
    }
}

#[derive(Debug, Traverse)]
pub struct PyBuffer {
    pub obj: PyObjectRef,
    #[pytraverse(skip)]
    pub desc: BufferDescriptor,
    #[pytraverse(skip)]
    methods: &'static BufferMethods,
}

impl PyBuffer {
    #[must_use]
    pub fn new(obj: PyObjectRef, desc: BufferDescriptor, methods: &'static BufferMethods) -> Self {
        #[cfg(debug_assertions)]
        let desc = desc.validate();

        let zelf = Self { obj, desc, methods };
        zelf.retain();
        zelf
    }

    #[must_use]
    pub fn as_contiguous(&self) -> Option<BorrowedValue<'_, [u8]>> {
        self.desc
            .is_contiguous()
            .then(|| unsafe { self.contiguous_unchecked() })
    }

    #[must_use]
    pub fn as_contiguous_mut(&self) -> Option<BorrowedValueMut<'_, [u8]>> {
        (!self.desc.readonly && self.desc.is_contiguous())
            .then(|| unsafe { self.contiguous_mut_unchecked() })
    }

    pub fn from_byte_vector(bytes: Vec<u8>, vm: &VirtualMachine) -> Self {
        let bytes_len = bytes.len();
        Self::new(
            PyPayload::into_pyobject(VecBuffer::from(bytes), vm),
            BufferDescriptor::simple(bytes_len, true),
            &VEC_BUFFER_METHODS,
        )
    }

    /// # Safety
    /// assume the buffer is contiguous
    #[must_use]
    pub unsafe fn contiguous_unchecked(&self) -> BorrowedValue<'_, [u8]> {
        self.obj_bytes()
    }

    /// # Safety
    /// assume the buffer is contiguous and writable
    #[must_use]
    pub unsafe fn contiguous_mut_unchecked(&self) -> BorrowedValueMut<'_, [u8]> {
        self.obj_bytes_mut()
    }

    pub fn append_to(&self, buf: &mut Vec<u8>) {
        if let Some(bytes) = self.as_contiguous() {
            buf.extend_from_slice(&bytes);
        } else {
            let bytes = &*self.obj_bytes();
            self.desc.for_each_segment(true, |range| {
                buf.extend_from_slice(&bytes[range.start as usize..range.end as usize])
            });
        }
    }

    pub fn contiguous_or_collect<R, F: FnOnce(&[u8]) -> R>(&self, f: F) -> R {
        let borrowed;
        let mut collected;
        let v = if let Some(bytes) = self.as_contiguous() {
            borrowed = bytes;
            &*borrowed
        } else {
            collected = vec![];
            self.append_to(&mut collected);
            &collected
        };
        f(v)
    }

    #[must_use]
    pub fn obj_as<T: PyObjectPayload>(&self) -> &Py<T> {
        unsafe { self.obj.downcast_unchecked_ref() }
    }

    #[must_use]
    pub fn obj_bytes(&self) -> BorrowedValue<'_, [u8]> {
        (self.methods.obj_bytes)(self)
    }

    #[must_use]
    pub fn obj_bytes_mut(&self) -> BorrowedValueMut<'_, [u8]> {
        (self.methods.obj_bytes_mut)(self)
    }

    pub fn release(&self) {
        (self.methods.release)(self)
    }

    pub fn retain(&self) {
        (self.methods.retain)(self)
    }

    // drop PyBuffer without calling release
    // after this function, the owner should use forget()
    // or wrap PyBuffer in the ManuallyDrop to prevent drop()
    pub(crate) unsafe fn drop_without_release(&mut self) {
        // SAFETY: requirements forwarded from caller
        unsafe {
            core::ptr::drop_in_place(&mut self.obj);
            core::ptr::drop_in_place(&mut self.desc);
        }
    }
}

impl Clone for PyBuffer {
    fn clone(&self) -> Self {
        self.retain();
        Self {
            obj: self.obj.clone(),
            desc: self.desc.clone(),
            methods: self.methods,
        }
    }
}

impl<'a> TryFromBorrowedObject<'a> for PyBuffer {
    fn try_from_borrowed_object(vm: &VirtualMachine, obj: &'a PyObject) -> PyResult<Self> {
        let cls = obj.class();
        if let Some(f) = cls.slots.as_buffer {
            return f(obj, vm);
        }
        if let Some(result) = try_c_buffer(obj, vm) {
            return result;
        }
        Err(vm.new_type_error(format!(
            "a bytes-like object is required, not '{}'",
            cls.name()
        )))
    }
}

impl Drop for PyBuffer {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Debug, Clone)]
pub struct BufferDescriptor {
    /// product(shape) * itemsize
    /// bytes length, but not the length for obj_bytes() even is contiguous
    pub len: usize,
    pub readonly: bool,
    pub itemsize: usize,
    pub format: Cow<'static, str>,
    /// (shape, stride, suboffset) for each dimension
    pub dim_desc: Vec<(usize, isize, isize)>,
    // TODO: flags
}

impl BufferDescriptor {
    #[must_use]
    pub fn simple(bytes_len: usize, readonly: bool) -> Self {
        Self {
            len: bytes_len,
            readonly,
            itemsize: 1,
            format: Cow::Borrowed("B"),
            dim_desc: vec![(bytes_len, 1, 0)],
        }
    }

    #[must_use]
    pub fn format(
        bytes_len: usize,
        readonly: bool,
        itemsize: usize,
        format: Cow<'static, str>,
    ) -> Self {
        Self {
            len: bytes_len,
            readonly,
            itemsize,
            format,
            dim_desc: vec![(bytes_len / itemsize, itemsize as isize, 0)],
        }
    }

    #[cfg(debug_assertions)]
    #[must_use]
    pub fn validate(self) -> Self {
        // ndim=0 is valid for scalar types (e.g., ctypes Structure)
        if self.ndim() == 0 {
            // Empty structures (len=0) can have itemsize=0
            if self.len > 0 {
                debug_assert_ne!(self.itemsize, 0);
            }
            debug_assert_eq!(self.itemsize, self.len);
        } else {
            let mut shape_product = 1;
            let has_zero_dim = self.dim_desc.iter().any(|(s, _, _)| *s == 0);
            for (shape, stride, suboffset) in self.dim_desc.iter().copied() {
                shape_product *= shape;
                debug_assert!(suboffset >= 0);
                // For empty arrays (any dimension is 0), strides can be 0
                if !has_zero_dim {
                    debug_assert_ne!(stride, 0);
                }
            }
            debug_assert_eq!(shape_product * self.itemsize, self.len);
        }
        self
    }

    #[must_use]
    pub fn ndim(&self) -> usize {
        self.dim_desc.len()
    }

    #[must_use]
    pub fn is_contiguous(&self) -> bool {
        if self.len == 0 {
            return true;
        }
        let mut sd = self.itemsize;
        for (shape, stride, _) in self.dim_desc.iter().copied().rev() {
            if shape > 1 && stride != sd as isize {
                return false;
            }
            sd *= shape;
        }
        true
    }

    /// this function do not check the bound
    /// panic if indices.len() != ndim
    #[must_use]
    pub fn fast_position(&self, indices: &[usize]) -> isize {
        let mut pos = 0;
        for (i, (_, stride, suboffset)) in indices
            .iter()
            .copied()
            .zip_eq(self.dim_desc.iter().copied())
        {
            pos += i as isize * stride + suboffset;
        }
        pos
    }

    /// panic if indices.len() != ndim
    pub fn position(&self, indices: &[isize], vm: &VirtualMachine) -> PyResult<isize> {
        let mut pos = 0;
        for (i, (shape, stride, suboffset)) in indices
            .iter()
            .copied()
            .zip_eq(self.dim_desc.iter().copied())
        {
            let i = i.wrapped_at(shape).ok_or_else(|| {
                vm.new_index_error(format!("index out of bounds on dimension {i}"))
            })?;
            pos += i as isize * stride + suboffset;
        }
        Ok(pos)
    }

    pub fn for_each_segment<F>(&self, try_contiguous: bool, mut f: F)
    where
        F: FnMut(Range<isize>),
    {
        if self.ndim() == 0 {
            f(0..self.itemsize as isize);
            return;
        }
        if try_contiguous && self.is_last_dim_contiguous() {
            self._for_each_segment::<_, true>(0, 0, &mut f);
        } else {
            self._for_each_segment::<_, false>(0, 0, &mut f);
        }
    }

    fn _for_each_segment<F, const CONTIGUOUS: bool>(&self, mut index: isize, dim: usize, f: &mut F)
    where
        F: FnMut(Range<isize>),
    {
        let (shape, stride, suboffset) = self.dim_desc[dim];
        if dim + 1 == self.ndim() {
            if CONTIGUOUS {
                f(index..index + (shape * self.itemsize) as isize);
            } else {
                for _ in 0..shape {
                    let pos = index + suboffset;
                    f(pos..pos + self.itemsize as isize);
                    index += stride;
                }
            }
            return;
        }
        for _ in 0..shape {
            self._for_each_segment::<F, CONTIGUOUS>(index + suboffset, dim + 1, f);
            index += stride;
        }
    }

    /// zip two BufferDescriptor with the same shape
    pub fn zip_eq<F>(&self, other: &Self, try_contiguous: bool, mut f: F)
    where
        F: FnMut(Range<isize>, Range<isize>) -> bool,
    {
        if self.ndim() == 0 {
            f(0..self.itemsize as isize, 0..other.itemsize as isize);
            return;
        }
        if try_contiguous && self.is_last_dim_contiguous() {
            self._zip_eq::<_, true>(other, 0, 0, 0, &mut f);
        } else {
            self._zip_eq::<_, false>(other, 0, 0, 0, &mut f);
        }
    }

    fn _zip_eq<F, const CONTIGUOUS: bool>(
        &self,
        other: &Self,
        mut a_index: isize,
        mut b_index: isize,
        dim: usize,
        f: &mut F,
    ) where
        F: FnMut(Range<isize>, Range<isize>) -> bool,
    {
        let (shape, a_stride, a_suboffset) = self.dim_desc[dim];
        let (_b_shape, b_stride, b_suboffset) = other.dim_desc[dim];
        debug_assert_eq!(shape, _b_shape);
        if dim + 1 == self.ndim() {
            if CONTIGUOUS {
                if f(
                    a_index..a_index + (shape * self.itemsize) as isize,
                    b_index..b_index + (shape * other.itemsize) as isize,
                ) {
                    return;
                }
            } else {
                for _ in 0..shape {
                    let a_pos = a_index + a_suboffset;
                    let b_pos = b_index + b_suboffset;
                    if f(
                        a_pos..a_pos + self.itemsize as isize,
                        b_pos..b_pos + other.itemsize as isize,
                    ) {
                        return;
                    }
                    a_index += a_stride;
                    b_index += b_stride;
                }
            }
            return;
        }

        for _ in 0..shape {
            self._zip_eq::<F, CONTIGUOUS>(
                other,
                a_index + a_suboffset,
                b_index + b_suboffset,
                dim + 1,
                f,
            );
            a_index += a_stride;
            b_index += b_stride;
        }
    }

    #[must_use]
    fn is_last_dim_contiguous(&self) -> bool {
        let (_, stride, suboffset) = self.dim_desc[self.ndim() - 1];
        suboffset == 0 && stride == self.itemsize as isize
    }

    #[must_use]
    pub fn is_zero_in_shape(&self) -> bool {
        self.dim_desc.iter().any(|(shape, _, _)| *shape == 0)
    }

    // TODO: support column-major order
}

pub trait BufferResizeGuard {
    type Resizable<'a>: 'a
    where
        Self: 'a;
    fn try_resizable_opt(&self) -> Option<Self::Resizable<'_>>;
    fn try_resizable(&self, vm: &VirtualMachine) -> PyResult<Self::Resizable<'_>> {
        self.try_resizable_opt().ok_or_else(|| {
            vm.new_buffer_error("Existing exports of data: object cannot be re-sized")
        })
    }
}

#[pyclass(module = false, name = "vec_buffer")]
#[derive(Debug, PyPayload)]
pub struct VecBuffer {
    data: PyMutex<Vec<u8>>,
}

#[pyclass(flags(BASETYPE, DISALLOW_INSTANTIATION))]
impl VecBuffer {
    pub fn take(&self) -> Vec<u8> {
        core::mem::take(&mut self.data.lock())
    }
}

impl From<Vec<u8>> for VecBuffer {
    fn from(data: Vec<u8>) -> Self {
        Self {
            data: PyMutex::new(data),
        }
    }
}

impl PyRef<VecBuffer> {
    #[must_use]
    pub fn into_pybuffer(self, readonly: bool) -> PyBuffer {
        let len = self.data.lock().len();
        PyBuffer::new(
            self.into(),
            BufferDescriptor::simple(len, readonly),
            &VEC_BUFFER_METHODS,
        )
    }

    #[must_use]
    pub fn into_pybuffer_with_descriptor(self, desc: BufferDescriptor) -> PyBuffer {
        PyBuffer::new(self.into(), desc, &VEC_BUFFER_METHODS)
    }
}

static VEC_BUFFER_METHODS: BufferMethods = BufferMethods {
    obj_bytes: |buffer| {
        PyMutexGuard::map_immutable(buffer.obj_as::<VecBuffer>().data.lock(), |x| x.as_slice())
            .into()
    },
    obj_bytes_mut: |buffer| {
        PyMutexGuard::map(buffer.obj_as::<VecBuffer>().data.lock(), |x| {
            x.as_mut_slice()
        })
        .into()
    },
    release: |_| {},
    retain: |_| {},
};

// ---- C-extension buffer protocol ------------------------------------------

/// Mirror of CPython's `Py_buffer` (layout must match crates/capi/src/buffer.rs).
#[repr(C)]
#[derive(Debug)]
pub struct CPyBuffer {
    pub buf: *mut c_void,
    pub obj: *mut PyObject,
    pub len: isize,
    pub itemsize: isize,
    pub readonly: c_int,
    pub ndim: c_int,
    pub format: *mut c_char,
    pub shape: *mut isize,
    pub strides: *mut isize,
    pub suboffsets: *mut isize,
    pub internal: *mut c_void,
}

// Safety: raw pointer fields are accessed only through the C-API, which is
// thread-safe per the Python buffer protocol contract.
unsafe impl Send for CPyBuffer {}
unsafe impl Sync for CPyBuffer {}

/// C buffer-protocol callbacks attached to a heap type through
/// `PyType::init_type_data`.
#[derive(Clone, Copy)]
pub struct CBufferSlots {
    pub getbuffer: unsafe extern "C" fn(*mut PyObject, *mut CPyBuffer, c_int) -> c_int,
    pub releasebuffer: Option<unsafe extern "C" fn(*mut PyObject, *mut CPyBuffer)>,
}

/// Internal wrapper that keeps a C exporter's buffer alive.
#[pyclass(module = false, name = "_c_exported_buffer")]
#[derive(Debug, PyPayload)]
pub struct CExportedBuffer {
    buf: NonNull<u8>,
    len: usize,
    exporter: PyObjectRef,
    view: Box<CPyBuffer>,
    refs: AtomicUsize,
}

// Safety: raw pointer (buf) is valid for the lifetime of the export; the
// C-API exporter manages thread safety per the buffer protocol contract.
unsafe impl Send for CExportedBuffer {}
unsafe impl Sync for CExportedBuffer {}

static C_BUFFER_METHODS: BufferMethods = BufferMethods {
    obj_bytes: |buffer| {
        let w = buffer.obj_as::<CExportedBuffer>();
        BorrowedValue::Ref(unsafe { core::slice::from_raw_parts(w.buf.as_ptr(), w.len) })
    },
    obj_bytes_mut: |buffer| {
        let w = buffer.obj_as::<CExportedBuffer>();
        BorrowedValueMut::RefMut(unsafe { core::slice::from_raw_parts_mut(w.buf.as_ptr(), w.len) })
    },
    release: |buffer| {
        let w = buffer.obj_as::<CExportedBuffer>();
        let prev = w.refs.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            let exporter = w.exporter.as_object().as_raw().cast_mut();
            if let Some(release) = w.exporter.class().get_type_data::<CBufferSlots>()
                && let Some(releasebuf) = release.releasebuffer
            {
                let view = core::ptr::addr_of!(*w.view) as *mut CPyBuffer;
                unsafe { releasebuf(exporter, view) };
            }
        }
    },
    retain: |buffer| {
        let w = buffer.obj_as::<CExportedBuffer>();
        w.refs.fetch_add(1, Ordering::AcqRel);
    },
};

#[pyclass]
impl CExportedBuffer {
    /// Wrap a C-exported `Py_buffer` (already filled by the exporter's
    /// `getbufferproc`) into a Rust `PyBuffer`. Takes ownership of the
    /// `view.obj` reference.
    fn into_pybuffer(view: CPyBuffer, vm: &VirtualMachine) -> PyResult<PyBuffer> {
        let buf = NonNull::new(view.buf.cast::<u8>())
            .ok_or_else(|| vm.new_buffer_error("C buffer has a NULL data pointer"))?;
        if view.obj.is_null() {
            return Err(
                vm.new_buffer_error("C exporter did not set Py_buffer.obj (unsupported exporter)")
            );
        }
        let exporter = unsafe { PyObjectRef::from_raw(NonNull::new_unchecked(view.obj)) };

        let len = view.len.max(0) as usize;
        let itemsize = view.itemsize.max(0) as usize;
        let readonly = view.readonly != 0;
        let format = if view.format.is_null() {
            Cow::Borrowed("B")
        } else {
            Cow::Owned(
                unsafe { CStr::from_ptr(view.format) }
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        let dim_desc = if view.ndim > 0 && !view.shape.is_null() {
            let ndim = view.ndim as usize;
            let shape = unsafe { core::slice::from_raw_parts(view.shape, ndim) };
            let strides = if view.strides.is_null() {
                None
            } else {
                Some(unsafe { core::slice::from_raw_parts(view.strides, ndim) })
            };
            let suboffsets = if view.suboffsets.is_null() {
                None
            } else {
                Some(unsafe { core::slice::from_raw_parts(view.suboffsets, ndim) })
            };
            shape
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    let stride = strides.map_or(itemsize as isize, |st| st[i]);
                    let sub = suboffsets.map_or(0, |so| so[i]);
                    (s as usize, stride, sub)
                })
                .collect()
        } else {
            vec![(
                if itemsize > 0 { len / itemsize } else { 0 },
                itemsize as isize,
                0,
            )]
        };
        let desc = BufferDescriptor {
            len,
            readonly,
            itemsize,
            format,
            dim_desc,
        };
        #[cfg(debug_assertions)]
        let desc = desc.validate();

        let wrapper = Self {
            buf,
            len,
            exporter,
            view: Box::new(view),
            refs: AtomicUsize::new(0),
        };
        Ok(PyBuffer::new(
            wrapper.into_pyobject(vm),
            desc,
            &C_BUFFER_METHODS,
        ))
    }
}

/// Attempt to obtain a `PyBuffer` from an object backed by a C exporter whose
/// type carries [`CBufferSlots`]. Used by the buffer-protocol lookup after the
/// native `as_buffer` slot has been consulted.
pub(crate) fn try_c_buffer(obj: &PyObject, vm: &VirtualMachine) -> Option<PyResult<PyBuffer>> {
    let cls = obj.class();
    let slots = cls.get_type_data::<CBufferSlots>()?;
    const PY_BUF_FULL_RO: c_int = 0x100 | 0x18 | 0x4;
    let mut view: CPyBuffer = unsafe { core::mem::zeroed() };
    let ret = unsafe { (slots.getbuffer)(obj.as_raw().cast_mut(), &raw mut view, PY_BUF_FULL_RO) };
    if ret != 0 {
        return Some(Err(vm.new_buffer_error("C exporter's getbuffer() failed")));
    }
    Some(CExportedBuffer::into_pybuffer(view, vm))
}
