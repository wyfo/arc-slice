use alloc::{
    alloc::{alloc, dealloc, handle_alloc_error},
    boxed::Box,
    vec::Vec,
};
use core::{
    alloc::{Layout, LayoutError},
    any::{Any, TypeId},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::{addr_of_mut, NonNull},
    sync::atomic::Ordering,
};

#[allow(unused_imports)]
use crate::msrv::{ConstPtrExt, NonNullExt, StrictProvenance};
use crate::{
    atomic,
    atomic::AtomicUsize,
    buffer::{ArrayPtr, Buffer, BufferMut, BufferWithMetadata, DynBuffer},
    macros::is,
    msrv::{ptr, BoxExt, NonZero},
    utils::slice_into_raw_parts,
};

const MAX_REFCOUNT: usize = isize::MAX as usize;
#[cfg(not(feature = "abort-on-refcount-overflow"))]
const SATURATED_REFCOUNT: usize = (isize::MIN / 2) as usize;

const CAPACITY_FLAG: usize = 1;
const CAPACITY_SHIFT: usize = 1;

// The structure needs to be repr(C) to allow pointer casting between `ErasedArc` and
// `ArcInner<B>`. `align(2)` is added to ensure the possibility of pointer tagging.
#[repr(C, align(2))]
struct ArcInner<B> {
    refcount: AtomicUsize,
    vtable_or_capacity: *const (),
    buffer: B,
}

#[repr(C)]
struct WithLength<B> {
    length: usize,
    buffer: B,
}

impl<T> WithLength<[T; 0]> {
    fn new() -> Self {
        Self {
            length: 0,
            buffer: [],
        }
    }

    fn as_mut_slice(&mut self) -> *mut [T] {
        // use `as_mut_slice` to avoid confusion with `BufferMut::as_mut_ptr`
        ptr::slice_from_raw_parts_mut(self.buffer.as_mut_slice().as_mut_ptr(), self.length)
    }
}

struct CompactVec<T> {
    start: NonNull<T>,
    capacity: NonZero<usize>,
}

impl<T> CompactVec<T> {
    unsafe fn to_vec(&self, length: usize) -> Vec<T> {
        unsafe { Vec::from_raw_parts(self.start.as_ptr(), length, self.capacity.get()) }
    }

    unsafe fn is_unique(_arc: ErasedArc) -> bool {
        true
    }

    #[allow(unstable_name_collisions)]
    unsafe fn take_buffer(
        buffer: NonNull<()>,
        arc: ErasedArc,
        type_id: TypeId,
        start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>>
    where
        T: Send + Sync + 'static,
    {
        let vec = &unsafe { arc.cast::<ArcInner<Self>>().as_ref() }.buffer;
        let capacity = vec.capacity.get();
        if is!({ type_id }, Vec<T>) {
            if start.cast::<T>() != vec.start {
                unsafe { ptr::copy(start.cast::<T>().as_ptr(), vec.start.as_ptr(), length) };
            }
            unsafe { buffer.cast().write(vec.to_vec(length)) };
        } else if is!({ type_id }, Box<[T]>) && length == capacity {
            let slice = ptr::slice_from_raw_parts_mut(vec.start.as_ptr(), capacity);
            unsafe { buffer.cast().write(Box::from_raw(slice)) };
        } else {
            return None;
        }
        drop(unsafe { Box::from_non_null(arc.cast::<ArcInner<MaybeUninit<Self>>>()) });
        Some(buffer)
    }
}

impl<T> Drop for CompactVec<T> {
    fn drop(&mut self) {
        drop(unsafe { Vec::from_raw_parts(self.start.as_ptr(), 0, self.capacity.get()) });
    }
}

impl<T> From<Vec<T>> for CompactVec<T> {
    fn from(value: Vec<T>) -> Self {
        assert!(!mem::needs_drop::<T>());
        let mut vec = ManuallyDrop::new(value);

        CompactVec {
            start: NonNull::new(vec.as_mut_ptr()).unwrap(),
            capacity: unsafe { NonZero::new_unchecked(vec.capacity()) },
        }
    }
}

type FullVec<T> = BufferWithMetadata<Vec<T>, ()>;

#[allow(clippy::type_complexity)]
struct ArcVTable {
    dealloc: unsafe fn(ErasedArc),
    is_unique: unsafe fn(ErasedArc) -> bool,
    get_metadata: Option<unsafe fn(ErasedArc, TypeId) -> Option<NonNull<()>>>,
    take_buffer:
        unsafe fn(NonNull<()>, ErasedArc, TypeId, NonNull<()>, usize) -> Option<NonNull<()>>,
}

impl ArcVTable {
    #[allow(unstable_name_collisions)]
    unsafe fn dealloc<B>(arc: ErasedArc) {
        drop(unsafe { Box::from_non_null(arc.cast::<ArcInner<B>>()) });
    }

    unsafe fn is_unique<T, B: Buffer<T>>(arc: ErasedArc) -> bool {
        unsafe { arc.cast::<ArcInner<B>>().as_ref() }
            .buffer
            .is_unique()
    }

    unsafe fn get_metadata<B: DynBuffer>(arc: ErasedArc, type_id: TypeId) -> Option<NonNull<()>> {
        unsafe { arc.cast::<ArcInner<B>>().as_ref() }
            .buffer
            .get_metadata(type_id)
    }

    #[allow(unstable_name_collisions)]
    unsafe fn take_buffer<T, B: DynBuffer + Buffer<T>>(
        buffer: NonNull<()>,
        arc: ErasedArc,
        type_id: TypeId,
        _start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>> {
        let inner = arc.cast::<ArcInner<B>>();
        if unsafe { inner.as_ref().buffer.as_slice().len() == length }
            && unsafe { B::take_buffer(addr_of_mut!((*inner.as_ptr()).buffer), type_id, buffer) }
        {
            drop(unsafe { Box::from_non_null(arc.cast::<ArcInner<MaybeUninit<B>>>()) });
            return Some(buffer);
        }
        None
    }

    fn new<T, B: DynBuffer + Buffer<T>>() -> &'static Self {
        if B::has_metadata() {
            &Self {
                dealloc: Self::dealloc::<B>,
                is_unique: Self::is_unique::<T, B>,
                get_metadata: Some(Self::get_metadata::<B>),
                take_buffer: Self::take_buffer::<T, B>,
            }
        } else {
            &Self {
                dealloc: Self::dealloc::<B>,
                is_unique: Self::is_unique::<T, B>,
                get_metadata: None,
                take_buffer: Self::take_buffer::<T, B>,
            }
        }
    }

    fn new_mut<T, B: DynBuffer + BufferMut<T>>() -> &'static Self {
        if B::has_metadata() {
            &Self {
                dealloc: Self::dealloc::<B>,
                is_unique: Self::is_unique::<T, B>,
                get_metadata: Some(Self::get_metadata::<B>),
                take_buffer: Self::take_buffer::<T, B>,
            }
        } else {
            &Self {
                dealloc: Self::dealloc::<B>,
                is_unique: Self::is_unique::<T, B>,
                get_metadata: None,
                take_buffer: Self::take_buffer::<T, B>,
            }
        }
    }

    fn new_compact_vec<T>() -> &'static Self
    where
        T: Send + Sync + 'static,
    {
        &Self {
            dealloc: Self::dealloc::<CompactVec<T>>,
            is_unique: CompactVec::<T>::is_unique,
            get_metadata: None,
            take_buffer: CompactVec::<T>::take_buffer,
        }
    }
}

enum VTableOrCapacity {
    VTable(&'static ArcVTable),
    Capacity(usize),
}

type ErasedArc = NonNull<ArcInner<()>>;
#[allow(missing_debug_implementations)]
pub struct Arc<T, const ANY_BUFFER: bool = true> {
    inner: ErasedArc,
    _phantom: PhantomData<T>,
}

impl<T, const ANY_BUFFER: bool> Arc<T, ANY_BUFFER> {
    fn slice_layout(len: usize) -> Result<Layout, LayoutError> {
        let inner_layout = if mem::needs_drop::<T>() {
            Layout::new::<ArcInner<WithLength<[T; 0]>>>()
        } else {
            Layout::new::<ArcInner<[T; 0]>>()
        };
        let (layout, _) = inner_layout.extend(Layout::array::<T>(len)?)?;
        Ok(layout)
    }

    fn allocate_slice<B>(
        capacity: usize,
        buffer: B,
        slice: impl Fn(&mut B) -> *mut [T],
    ) -> (Self, NonNull<T>) {
        let layout = Self::slice_layout(capacity).expect("capacity overflow");
        let mut inner_ptr = NonNull::new(unsafe { alloc(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout))
            .cast();
        let inner = ArcInner {
            refcount: AtomicUsize::new(1),
            vtable_or_capacity: ptr::without_provenance(
                CAPACITY_FLAG | (capacity << CAPACITY_SHIFT),
            ),
            buffer,
        };
        unsafe { inner_ptr.write(inner) };
        let arc = Self {
            inner: inner_ptr.cast(),
            _phantom: PhantomData,
        };
        let slice_ptr = slice(&mut unsafe { inner_ptr.as_mut() }.buffer);
        (arc, NonNull::new(slice_ptr.cast()).unwrap())
    }

    pub(crate) fn with_capacity(capacity: usize) -> (Self, NonNull<T>) {
        if mem::needs_drop::<T>() {
            Self::allocate_slice(capacity, [], |b| b.as_mut())
        } else {
            Self::allocate_slice(capacity, WithLength::new(), |b| b.as_mut_slice())
        }
    }

    pub(crate) fn new(slice: &[T]) -> (Self, NonNull<T>)
    where
        T: Copy,
    {
        let (arc, start) = Self::with_capacity(slice.len());
        unsafe { ptr::copy_nonoverlapping(slice.as_ptr(), start.as_ptr(), slice.len()) };
        (arc, start)
    }

    pub(crate) fn new_array<const N: usize>(array: [T; N]) -> (Self, NonNull<T>) {
        let array = ManuallyDrop::new(array);
        let (arc, start) = Self::with_capacity(N);
        unsafe { ptr::copy_nonoverlapping(array.as_ptr(), start.as_ptr(), N) };
        (arc, start)
    }

    pub(crate) fn into_raw(self) -> NonNull<()> {
        ManuallyDrop::new(self).inner.cast()
    }

    pub(crate) unsafe fn from_raw(ptr: NonNull<()>) -> Self {
        Self {
            inner: ptr.cast(),
            _phantom: PhantomData,
        }
    }

    fn vtable_or_capa(&self) -> VTableOrCapacity {
        let ptr = unsafe { self.inner.as_ref().vtable_or_capacity };
        if ANY_BUFFER && ptr.addr() & CAPACITY_FLAG == 0 {
            VTableOrCapacity::VTable(unsafe { &*ptr.cast() })
        } else {
            VTableOrCapacity::Capacity(ptr.addr() >> CAPACITY_SHIFT)
        }
    }

    pub(crate) fn is_unique(&self) -> bool {
        let inner = unsafe { self.inner.as_ref() };
        inner.refcount.load(Ordering::Relaxed) == 1
            && match self.vtable_or_capa() {
                VTableOrCapacity::VTable(vtable) => unsafe { (vtable.is_unique)(self.inner) },
                VTableOrCapacity::Capacity(_) => true,
            }
    }

    pub(crate) fn get_metadata<M: Any>(&self) -> Option<&M> {
        match self.vtable_or_capa() {
            VTableOrCapacity::VTable(vtable) => unsafe {
                let metadata = vtable.get_metadata?(self.inner, TypeId::of::<M>())?;
                Some(metadata.cast().as_ref())
            },
            VTableOrCapacity::Capacity(_) => None,
        }
    }

    pub(crate) fn take_buffer<B: Buffer<T>>(
        self,
        start: NonNull<T>,
        length: usize,
    ) -> Result<B, Self> {
        let inner = unsafe { self.inner.as_ref() };
        if inner.refcount.load(Ordering::Acquire) != 1 {
            return Err(self);
        }
        match self.vtable_or_capa() {
            VTableOrCapacity::VTable(vtable) => {
                let mut buffer = MaybeUninit::<B>::uninit();
                let buffer_ptr = NonNull::new(buffer.as_mut_ptr()).unwrap().cast();
                let type_id = TypeId::of::<B>();
                if let Some(buffer_ptr) = unsafe {
                    (vtable.take_buffer)(buffer_ptr, self.inner, type_id, start.cast(), length)
                } {
                    return unsafe { buffer_ptr.cast().read() };
                }
            }
            VTableOrCapacity::Capacity(capacity) => {
                if length == capacity {
                    let slice = ptr::slice_from_raw_parts_mut(start.as_ptr(), length);
                    if let Some(buffer) = unsafe { B::try_from_array(ArrayPtr(slice)) } {
                        let layout = Self::slice_layout(capacity).unwrap();
                        unsafe { dealloc(self.inner.as_ptr().cast(), layout) };
                        return Ok(buffer);
                    }
                }
            }
        }
        Err(self)
    }

    unsafe fn deallocate(&mut self) {
        match self.vtable_or_capa() {
            VTableOrCapacity::VTable(vtable) => unsafe { (vtable.dealloc)(self.inner) },
            VTableOrCapacity::Capacity(capacity) => {
                if mem::needs_drop::<T>() {
                    let inner =
                        unsafe { self.inner.cast::<ArcInner<WithLength<[T; 0]>>>().as_mut() };
                    unsafe { ptr::drop_in_place(inner.buffer.as_mut_slice()) };
                }
                let layout = Self::slice_layout(capacity).unwrap();
                unsafe { dealloc(self.inner.as_ptr().cast(), layout) };
            }
        }
    }

    pub(crate) fn drop(self, unique_hint: bool) {
        let inner = unsafe { self.inner.as_ref() };
        // There is no weak reference, so refcount equal to one means the Arc is truly unique.
        // Acquire ordering synchronizes with a potential release decrease of the refcount,
        // ensuring the data has been used before the following deallocation.
        if unique_hint && inner.refcount.load(Ordering::Acquire) == 1 {
            unsafe { ManuallyDrop::new(self).deallocate() };
        }
        // otherwise, the Arc is normally dropped
    }
}

impl<T> Arc<T> {
    fn allocate_buffer<B>(vtable: &'static ArcVTable, buffer: B) -> ArcGuard<B> {
        ArcGuard(ManuallyDrop::new(Box::new(ArcInner {
            refcount: AtomicUsize::new(1),
            vtable_or_capacity: ptr::from_ref(vtable).cast(),
            buffer,
        })))
    }

    pub(crate) fn new_vec(vec: Vec<T>) -> Self
    where
        T: Send + Sync + 'static,
    {
        if mem::needs_drop::<T>() {
            return Self::new_buffer(FullVec::new(vec, ())).0;
        }
        Self::allocate_buffer(ArcVTable::new_compact_vec::<T>(), CompactVec::from(vec)).into()
    }

    pub(crate) fn new_buffer<B: DynBuffer + Buffer<T>>(buffer: B) -> (Self, NonNull<T>, usize) {
        let arc = Self::allocate_buffer(ArcVTable::new::<T, B>(), buffer);
        let (start, length) = slice_into_raw_parts(arc.as_slice());
        (arc.into(), start, length)
    }

    pub(crate) fn new_buffer_mut<B: DynBuffer + BufferMut<T>>(
        buffer: B,
    ) -> (Self, NonNull<T>, usize, usize) {
        let mut arc = Self::allocate_buffer(ArcVTable::new::<T, B>(), buffer);
        let start = arc.as_mut_ptr();
        let length = arc.len();
        let capacity = arc.capacity();
        (arc.into(), start, length, capacity)
    }

    #[allow(unstable_name_collisions)]
    pub(crate) fn promote_vec(vec: Vec<T>) -> PromoteGuard<T>
    where
        T: Send + Sync + 'static,
    {
        fn guard<T, B>(vtable: &'static ArcVTable, buffer: B) -> PromoteGuard<T> {
            let arc = Box::new(ArcInner {
                refcount: AtomicUsize::new(2),
                vtable_or_capacity: ptr::from_ref(vtable).cast(),
                buffer,
            });
            PromoteGuard {
                arc: Box::into_non_null(arc).cast(),
                _phantom: PhantomData,
            }
        }
        if mem::needs_drop::<T>() {
            guard(ArcVTable::new::<T, FullVec<T>>(), FullVec::new(vec, ()))
        } else {
            guard(ArcVTable::new_compact_vec::<T>(), CompactVec::from(vec))
        }
    }
}

unsafe impl<T: Send + Sync, const ANY_BUFFER: bool> Send for Arc<T, ANY_BUFFER> {}
unsafe impl<T: Send + Sync, const ANY_BUFFER: bool> Sync for Arc<T, ANY_BUFFER> {}

impl<T, const ANY_BUFFER: bool> Drop for Arc<T, ANY_BUFFER> {
    fn drop(&mut self) {
        let inner = unsafe { self.inner.as_ref() };
        // See `Arc` documentation
        let prev_refcount = inner.refcount.fetch_sub(1, Ordering::Release);
        if prev_refcount == 1 {
            atomic::fence(Ordering::Acquire);
            unsafe { self.deallocate() }
        }
        // Saturate the refcount in no_std, as in Linux refcount
        #[cfg(not(feature = "abort-on-refcount-overflow"))]
        if prev_refcount > MAX_REFCOUNT {
            inner.refcount.store(SATURATED_REFCOUNT, Ordering::Relaxed);
        }
    }
}

impl<T, const ANY_BUFFER: bool> Clone for Arc<T, ANY_BUFFER> {
    fn clone(&self) -> Self {
        let inner = unsafe { self.inner.as_ref() };
        // See `Arc` documentation
        let old_size = inner.refcount.fetch_add(1, Ordering::Relaxed);
        if old_size > MAX_REFCOUNT {
            // Saturate the refcount in no_std, as in Linux refcount
            #[cfg(feature = "abort-on-refcount-overflow")]
            crate::utils::abort();
            #[cfg(not(feature = "abort-on-refcount-overflow"))]
            inner.refcount.store(SATURATED_REFCOUNT, Ordering::Relaxed);
        }
        Self {
            inner: self.inner,
            _phantom: PhantomData,
        }
    }
}

struct ArcGuard<B>(ManuallyDrop<Box<ArcInner<B>>>);

impl<B> Deref for ArcGuard<B> {
    type Target = B;
    fn deref(&self) -> &Self::Target {
        &self.0.buffer
    }
}

impl<B> DerefMut for ArcGuard<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.buffer
    }
}

impl<B> Drop for ArcGuard<B> {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.0) };
    }
}

#[allow(unstable_name_collisions)]
impl<T, B> From<ArcGuard<B>> for Arc<T> {
    fn from(value: ArcGuard<B>) -> Self {
        let mut guard = ManuallyDrop::new(value);
        Self {
            inner: Box::into_non_null(unsafe { ManuallyDrop::take(&mut guard.0) }).cast(),
            _phantom: PhantomData,
        }
    }
}

pub(crate) struct PromoteGuard<T> {
    arc: NonNull<()>,
    _phantom: PhantomData<T>,
}

impl<T> PromoteGuard<T> {
    pub(crate) fn as_ptr(&self) -> *mut () {
        self.arc.as_ptr()
    }
}

impl<T> Drop for PromoteGuard<T> {
    fn drop(&mut self) {
        if mem::needs_drop::<T>() {
            drop(unsafe { Box::from_raw(self.arc.as_ptr().cast::<MaybeUninit<FullVec<T>>>()) });
        } else {
            drop(unsafe { Box::from_raw(self.arc.as_ptr().cast::<MaybeUninit<CompactVec<T>>>()) });
        }
    }
}

impl<T> From<PromoteGuard<T>> for Arc<T> {
    fn from(value: PromoteGuard<T>) -> Self {
        unsafe { Self::from_raw(value.arc) }
    }
}
