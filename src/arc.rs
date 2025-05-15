use alloc::{
    alloc::{alloc, alloc_zeroed, dealloc, handle_alloc_error},
    boxed::Box,
    slice,
    vec::Vec,
};
use core::{
    alloc::{Layout, LayoutError},
    any::{Any, TypeId},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::{addr_of_mut, NonNull},
    sync::atomic::Ordering,
};

#[allow(unused_imports)]
use crate::msrv::{BoxExt, ConstPtrExt, NonNullExt, StrictProvenance};
use crate::{
    atomic,
    atomic::AtomicUsize,
    buffer::{ArrayPtr, Buffer, BufferMut, BufferMutExt, BufferWithMetadata, DynBuffer},
    error::TryReserveError,
    macros::is,
    msrv::{ptr, NonZero, SubPtrExt},
    slice_mut::TryReserveResult,
    utils::{assert_checked, slice_into_raw_parts, NewChecked, UnwrapChecked},
    vtable::{generic_take_buffer, VTable},
};

const MAX_REFCOUNT: usize = isize::MAX as usize;
#[cfg(not(feature = "abort-on-refcount-overflow"))]
const SATURATED_REFCOUNT: usize = (isize::MIN / 2) as usize;

const VTABLE_FLAG: usize = !(usize::MAX >> 1);
const VTABLE_SHIFT: usize = 1;

// The structure needs to be repr(C) to allow pointer casting between `ErasedArc` and
// `ArcInner<B>`. `align(2)` is added to ensure the possibility of pointer tagging.
#[repr(C, align(2))]
struct ArcInner<B> {
    refcount: AtomicUsize,
    vtable_or_capacity: *const (),
    buffer: B,
}

impl<B> ArcInner<B> {
    fn incr_refcount(&self) {
        // See `Arc` documentation
        let old_size = self.refcount.fetch_add(1, Ordering::Relaxed);
        if old_size > MAX_REFCOUNT {
            // Saturate the refcount in no_std, as in Linux refcount
            #[cfg(feature = "abort-on-refcount-overflow")]
            crate::utils::abort();
            #[cfg(not(feature = "abort-on-refcount-overflow"))]
            self.refcount.store(SATURATED_REFCOUNT, Ordering::Relaxed);
        }
    }

    fn is_unique(&self) -> bool {
        self.refcount.load(Ordering::Acquire) == 1
    }

    fn decr_refcount(&self) -> bool {
        // See `Arc` documentation
        let prev_refcount = self.refcount.fetch_sub(1, Ordering::Release);
        if prev_refcount == 1 {
            atomic::fence(Ordering::Acquire);
            return true;
        }
        // Saturate the refcount in no_std, as in Linux refcount
        #[cfg(not(feature = "abort-on-refcount-overflow"))]
        if prev_refcount > MAX_REFCOUNT {
            self.refcount.store(SATURATED_REFCOUNT, Ordering::Relaxed);
        }
        false
    }
}

type ErasedArc = NonNull<ArcInner<()>>;

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
}

struct CompactVec<T> {
    start: NonNull<T>,
    capacity: NonZero<usize>,
}

impl<T> CompactVec<T> {
    unsafe fn to_vec(&self, length: usize) -> Vec<T> {
        unsafe { Vec::from_raw_parts(self.start.as_ptr(), length, self.capacity.get()) }
    }

    unsafe fn is_buffer_unique(ptr: *const ()) -> bool {
        unsafe { &*ptr.cast::<ArcInner<Self>>() }.is_unique()
    }

    unsafe fn get_metadata(_ptr: *const (), _type_id: TypeId) -> Option<NonNull<()>> {
        None
    }

    unsafe fn take_buffer(
        buffer: NonNull<()>,
        ptr: *const (),
        type_id: TypeId,
        start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>>
    where
        T: Send + Sync + 'static,
    {
        let inner = unsafe { vtable::check_unique::<Self>(ptr) }?;
        let vec = &unsafe { &*inner }.buffer;
        let capacity = vec.capacity.get();
        if is!({ type_id }, Vec<T>) {
            if start.cast::<T>() != vec.start {
                let start = start.cast::<T>().as_ptr();
                unsafe { ptr::copy(start, vec.start.as_ptr(), length) };
            }
            unsafe { buffer.cast().write(vec.to_vec(length)) };
        } else if is!({ type_id }, Box<[T]>) && length == capacity {
            let slice = ptr::slice_from_raw_parts_mut(vec.start.as_ptr(), capacity);
            unsafe { buffer.cast().write(Box::from_raw(slice)) };
        } else {
            return None;
        }
        drop(unsafe { Box::from_raw(inner.cast::<ArcInner<MaybeUninit<Self>>>()) });
        Some(buffer)
    }

    #[allow(unstable_name_collisions)]
    unsafe fn capacity(ptr: *const (), start: NonNull<()>) -> usize {
        // MSRV 1.65 let-else
        let buffer = match unsafe { vtable::check_unique::<Self>(ptr) } {
            Some(inner) => &unsafe { &*inner }.buffer,
            None => return usize::MAX,
        };
        let offset = unsafe { start.cast().sub_ptr(buffer.start) };
        buffer.capacity.get() - offset
    }

    #[allow(unstable_name_collisions)]
    unsafe fn try_reserve(
        ptr: NonNull<()>,
        start: NonNull<()>,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<()>
    where
        T: Send + Sync + 'static,
    {
        struct ArcCompactVec<T> {
            arc: ManuallyDrop<Box<ArcInner<CompactVec<T>>>>,
            length: usize,
        }
        unsafe impl<T: Send + Sync> Send for ArcCompactVec<T> {}
        unsafe impl<T: Send + Sync> Sync for ArcCompactVec<T> {}
        impl<T: Send + Sync + 'static> Buffer<T> for ArcCompactVec<T> {
            fn as_slice(&self) -> &[T] {
                unsafe { slice::from_raw_parts(self.arc.buffer.start.as_ptr(), self.length) }
            }
        }
        unsafe impl<T: Send + Sync + 'static> BufferMut<T> for ArcCompactVec<T> {
            fn as_mut_ptr(&mut self) -> NonNull<T> {
                self.arc.buffer.start
            }
            fn len(&self) -> usize {
                self.length
            }
            fn capacity(&self) -> usize {
                self.arc.buffer.capacity.get()
            }
            unsafe fn set_len(&mut self, len: usize) -> bool {
                self.length = len;
                true
            }
            fn reserve(&mut self, additional: usize) -> bool {
                let (start, capacity) = unsafe {
                    self.realloc(additional, self.arc.buffer.start.cast(), Layout::array::<T>)
                };
                self.arc.buffer.start = start.cast();
                self.arc.buffer.capacity = unsafe { NonZero::new_unchecked(capacity) };
                true
            }
        }
        let arc = ManuallyDrop::new(unsafe { Box::from_non_null(ptr.cast::<ArcInner<Self>>()) });
        let offset = unsafe { start.cast().sub_ptr(arc.buffer.start) };
        let mut buffer = ArcCompactVec {
            arc,
            length: offset + length,
        };
        let (capacity, start) =
            unsafe { buffer.try_reserve_impl(offset, length, additional, allocate) };
        (capacity, start.cast())
    }
}

impl<T> Drop for CompactVec<T> {
    fn drop(&mut self) {
        drop(unsafe { self.to_vec(0) });
    }
}

impl<T> From<Vec<T>> for CompactVec<T> {
    fn from(value: Vec<T>) -> Self {
        assert_checked(!mem::needs_drop::<T>());
        let mut vec = ManuallyDrop::new(value);

        CompactVec {
            start: NonNull::new_checked(vec.as_mut_ptr()),
            capacity: unsafe { NonZero::new_unchecked(vec.capacity()) },
        }
    }
}

type FullVec<T> = BufferWithMetadata<Vec<T>, ()>;

pub(crate) mod vtable {
    use alloc::boxed::Box;
    use core::{
        any::TypeId,
        mem,
        mem::MaybeUninit,
        ptr::{addr_of_mut, NonNull},
    };

    #[allow(unused_imports)]
    use crate::msrv::ConstPtrExt;
    use crate::{
        arc::{ArcInner, CompactVec},
        buffer::{Buffer, BufferMut, BufferMutExt, DynBuffer},
        error::TryReserveError,
        macros::{is, is_not},
        msrv::SubPtrExt,
        slice_mut::TryReserveResult,
        utils::slice_into_raw_parts,
        vtable::{no_capacity, VTable},
    };

    unsafe fn deallocate<B>(ptr: *mut ()) {
        mem::drop(unsafe { Box::from_raw(ptr.cast::<ArcInner<B>>()) });
    }
    unsafe fn is_buffer_unique<T, B: Buffer<T>>(ptr: *const ()) -> bool {
        let inner = unsafe { &*ptr.cast::<ArcInner<B>>() };
        inner.is_unique() && inner.buffer.is_unique()
    }

    unsafe fn get_metadata<B: DynBuffer>(ptr: *const (), type_id: TypeId) -> Option<NonNull<()>> {
        if is!(B::Metadata, ()) || is_not!({ type_id }, B::Metadata) {
            return None;
        }
        let buffer = &unsafe { &*ptr.cast::<ArcInner<B>>() }.buffer;
        Some(NonNull::from(buffer.get_metadata()).cast())
    }

    pub(super) unsafe fn check_unique<B>(ptr: *const ()) -> Option<*mut ArcInner<B>> {
        unsafe { &*ptr.cast::<ArcInner<B>>() }
            .is_unique()
            .then(|| ptr.cast_mut().cast())
    }

    unsafe fn take_buffer<T, B: DynBuffer + Buffer<T>>(
        buffer: NonNull<()>,
        ptr: *const (),
        type_id: TypeId,
        _start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>> {
        let inner = unsafe { check_unique::<B>(ptr) }?;
        if is_not!({ type_id }, B::Buffer) || unsafe { &*inner }.buffer.as_slice().len() != length {
            return None;
        }
        unsafe { B::take_buffer(addr_of_mut!((*inner).buffer), buffer) };
        mem::drop(unsafe { Box::from_raw(inner.cast::<ArcInner<MaybeUninit<B>>>()) });
        Some(buffer)
    }

    #[allow(unstable_name_collisions)]
    unsafe fn capacity<T, B: BufferMut<T>>(ptr: *const (), start: NonNull<()>) -> usize {
        let buffer = match unsafe { check_unique::<B>(ptr) } {
            Some(inner) => &unsafe { &*inner }.buffer,
            None => return usize::MAX,
        };
        let (buffer_start, _) = slice_into_raw_parts(buffer.as_slice());
        let offset = unsafe { start.cast().sub_ptr(buffer_start) };
        buffer.capacity() - offset
    }

    #[allow(unstable_name_collisions)]
    unsafe fn try_reserve<T, B: BufferMut<T>>(
        ptr: NonNull<()>,
        start: NonNull<()>,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<()> {
        let buffer = &mut unsafe { ptr.cast::<ArcInner<B>>().as_mut() }.buffer;
        let (buffer_start, buffer_length) = slice_into_raw_parts(buffer.as_slice());
        let offset = unsafe { start.cast().sub_ptr(buffer_start) };
        if mem::needs_drop::<T>() && buffer_length != offset + length {
            return (Err(TryReserveError::Unsupported), start);
        }
        let (capacity, start) =
            unsafe { buffer.try_reserve_impl(offset, length, additional, allocate) };
        (capacity, start.cast())
    }

    #[cfg(feature = "raw-buffer")]
    unsafe fn drop<B>(ptr: *const ()) {
        let inner = unsafe { &*ptr.cast::<ArcInner<B>>() };
        if inner.decr_refcount() {
            unsafe { deallocate::<B>(ptr.cast_mut()) }
        }
    }

    #[cfg(feature = "raw-buffer")]
    unsafe fn drop_with_unique_hint<B>(ptr: *const ()) {
        let inner = unsafe { &*ptr.cast::<ArcInner<B>>() };
        if inner.is_unique() || inner.decr_refcount() {
            unsafe { deallocate::<B>(ptr.cast_mut()) }
        }
    }

    #[cfg(feature = "raw-buffer")]
    unsafe fn clone(ptr: *const ()) {
        unsafe { &*ptr.cast::<ArcInner<()>>() }.incr_refcount();
    }

    #[cfg(feature = "raw-buffer")]
    unsafe fn into_arc(ptr: *const ()) -> Option<NonNull<()>> {
        NonNull::new(ptr.cast_mut())
    }

    pub(crate) fn new<T, B: DynBuffer + Buffer<T>>() -> &'static VTable {
        &VTable {
            deallocate: deallocate::<B>,
            is_buffer_unique: is_buffer_unique::<T, B>,
            get_metadata: get_metadata::<B>,
            take_buffer: take_buffer::<T, B>,
            capacity: no_capacity,
            try_reserve: None,
            #[cfg(feature = "raw-buffer")]
            drop: drop::<B>,
            #[cfg(feature = "raw-buffer")]
            drop_with_unique_hint: drop_with_unique_hint::<B>,
            #[cfg(feature = "raw-buffer")]
            clone,
            #[cfg(feature = "raw-buffer")]
            into_arc,
        }
    }

    pub(crate) fn new_mut<T, B: DynBuffer + BufferMut<T>>() -> &'static VTable {
        &VTable {
            deallocate: deallocate::<B>,
            is_buffer_unique: is_buffer_unique::<T, B>,
            get_metadata: get_metadata::<B>,
            take_buffer: take_buffer::<T, B>,
            capacity: capacity::<T, B>,
            try_reserve: Some(try_reserve::<T, B>),
            #[cfg(feature = "raw-buffer")]
            drop: drop::<B>,
            #[cfg(feature = "raw-buffer")]
            drop_with_unique_hint: drop_with_unique_hint::<B>,
            #[cfg(feature = "raw-buffer")]
            clone,
            #[cfg(feature = "raw-buffer")]
            into_arc,
        }
    }

    pub(super) fn new_compact_vec<T>() -> &'static VTable
    where
        T: Send + Sync + 'static,
    {
        &VTable {
            deallocate: deallocate::<CompactVec<T>>,
            is_buffer_unique: CompactVec::<T>::is_buffer_unique,
            get_metadata: CompactVec::<T>::get_metadata,
            take_buffer: CompactVec::<T>::take_buffer,
            capacity: CompactVec::<T>::capacity,
            try_reserve: Some(CompactVec::<T>::try_reserve),
            #[cfg(feature = "raw-buffer")]
            drop: drop::<CompactVec<T>>,
            #[cfg(feature = "raw-buffer")]
            drop_with_unique_hint: drop_with_unique_hint::<CompactVec<T>>,
            #[cfg(feature = "raw-buffer")]
            clone,
            #[cfg(feature = "raw-buffer")]
            into_arc,
        }
    }

    #[cfg(feature = "raw-buffer")]
    pub(crate) fn new_vec<T>() -> &'static VTable
    where
        T: Send + Sync + 'static,
    {
        if mem::needs_drop::<T>() {
            new::<T, super::FullVec<T>>()
        } else {
            new_compact_vec::<T>()
        }
    }
}

enum VTableOrCapacity {
    VTable(&'static VTable),
    Capacity(usize),
}

#[allow(missing_debug_implementations)]
pub struct Arc<T, const ANY_BUFFER: bool = true> {
    inner: ErasedArc,
    _phantom: PhantomData<T>,
}

impl<T, const ANY_BUFFER: bool> Arc<T, ANY_BUFFER> {
    fn slice_layout(capacity: usize) -> Result<Layout, LayoutError> {
        let inner_layout = if mem::needs_drop::<T>() {
            Layout::new::<ArcInner<WithLength<[T; 0]>>>()
        } else {
            Layout::new::<ArcInner<[T; 0]>>()
        };
        let (layout, _) = inner_layout.extend(Layout::array::<T>(capacity)?)?;
        Ok(layout)
    }

    fn allocate_slice<B, const ZEROED: bool>(capacity: usize, buffer: B) -> Self {
        let layout = Self::slice_layout(capacity).expect("capacity overflow");
        let inner_ptr = NonNull::new(unsafe {
            if ZEROED {
                alloc_zeroed(layout)
            } else {
                alloc(layout)
            }
        })
        .unwrap_or_else(|| handle_alloc_error(layout))
        .cast();
        let inner = ArcInner {
            refcount: AtomicUsize::new(1),
            vtable_or_capacity: ptr::without_provenance(capacity),
            buffer,
        };
        unsafe { inner_ptr.write(inner) };
        Self {
            inner: inner_ptr.cast(),
            _phantom: PhantomData,
        }
    }

    pub(crate) fn with_capacity<const ZEROED: bool>(capacity: usize) -> (Self, NonNull<T>) {
        let arc = if mem::needs_drop::<T>() {
            Self::allocate_slice::<WithLength<[T; 0]>, ZEROED>(capacity, WithLength::new())
        } else {
            Self::allocate_slice::<[T; 0], ZEROED>(capacity, [])
        };
        let start = unsafe { arc.slice_start() };
        (arc, start)
    }

    unsafe fn slice_start(&self) -> NonNull<T> {
        NonNull::new_checked(if mem::needs_drop::<T>() {
            let inner = self.inner.cast::<ArcInner<WithLength<[T; 0]>>>().as_ptr();
            unsafe { addr_of_mut!((*inner).buffer.buffer) }
        } else {
            let inner = self.inner.cast::<ArcInner<[T; 0]>>().as_ptr();
            unsafe { addr_of_mut!((*inner).buffer) }
        })
        .cast()
    }

    unsafe fn slice_length(&self) -> Option<usize> {
        if mem::needs_drop::<T>() {
            let inner = unsafe { self.inner.cast::<ArcInner<WithLength<[T; 0]>>>().as_ref() };
            Some(inner.buffer.length)
        } else {
            None
        }
    }

    pub(crate) fn new(slice: &[T]) -> (Self, NonNull<T>)
    where
        T: Copy,
    {
        let (arc, start) = Self::with_capacity::<false>(slice.len());
        unsafe { ptr::copy_nonoverlapping(slice.as_ptr(), start.as_ptr(), slice.len()) };
        (arc, start)
    }

    pub(crate) fn new_array<const N: usize>(array: [T; N]) -> (Self, NonNull<T>) {
        let array = ManuallyDrop::new(array);
        let (arc, start) = Self::with_capacity::<false>(N);
        unsafe { ptr::copy_nonoverlapping(array.as_ptr(), start.as_ptr(), N) };
        (arc, start)
    }

    fn as_ptr(&self) -> *const () {
        self.inner.as_ptr().cast()
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

    pub(crate) fn is_unique(&mut self) -> bool {
        unsafe { self.inner.as_ref() }.is_unique()
    }

    fn vtable_or_capacity(&self) -> VTableOrCapacity {
        let ptr = unsafe { self.inner.as_ref().vtable_or_capacity };
        if ANY_BUFFER && ptr.addr() & VTABLE_FLAG != 0 {
            VTableOrCapacity::VTable(unsafe { &*ptr.with_addr(ptr.addr() << VTABLE_SHIFT).cast() })
        } else {
            VTableOrCapacity::Capacity(ptr.addr())
        }
    }

    #[cfg(feature = "raw-buffer")]
    pub(crate) fn vtable(&self) -> Option<&'static VTable> {
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => Some(vtable),
            VTableOrCapacity::Capacity(_) => None,
        }
    }

    pub(crate) fn is_buffer_unique(&self) -> bool {
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => unsafe { (vtable.is_buffer_unique)(self.as_ptr()) },
            VTableOrCapacity::Capacity(_) => unsafe { self.inner.as_ref() }.is_unique(),
        }
    }

    pub(crate) fn get_metadata<M: Any>(&self) -> Option<&M> {
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => unsafe {
                let metadata = (vtable.get_metadata)(self.as_ptr(), TypeId::of::<M>())?;
                Some(metadata.cast().as_ref())
            },
            VTableOrCapacity::Capacity(_) => None,
        }
    }

    pub(crate) unsafe fn take_buffer<B: Buffer<T>, const UNIQUE: bool>(
        self,
        start: NonNull<T>,
        length: usize,
    ) -> Result<B, Self> {
        let mut this = ManuallyDrop::new(self);
        match this.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => {
                if let Some(buffer) =
                    unsafe { generic_take_buffer::<B>(this.as_ptr(), vtable, start.cast(), length) }
                {
                    return Ok(buffer);
                }
            }
            VTableOrCapacity::Capacity(capacity) => {
                if B::is_array() && (UNIQUE || this.is_unique()) && length == capacity {
                    let slice = ptr::slice_from_raw_parts_mut(start.as_ptr(), length);
                    if let Some(buffer) = unsafe { B::try_from_array(ArrayPtr(slice)) } {
                        let layout = unsafe { Self::slice_layout(capacity).unwrap_unchecked() };
                        unsafe { dealloc(this.inner.as_ptr().cast(), layout) };
                        return Ok(buffer);
                    }
                }
            }
        }
        Err(ManuallyDrop::into_inner(this))
    }

    #[allow(unstable_name_collisions)]
    pub(crate) unsafe fn capacity(&mut self, start: NonNull<T>) -> Option<usize> {
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => {
                Some(unsafe { (vtable.capacity)(self.as_ptr(), start.cast()) })
                    .filter(|&capacity| capacity != usize::MAX)
            }
            VTableOrCapacity::Capacity(capacity) => self
                .is_unique()
                .then(|| capacity - unsafe { start.sub_ptr(self.slice_start()) }),
        }
    }

    #[allow(unstable_name_collisions)]
    pub(crate) unsafe fn try_reserve<const UNIQUE: bool>(
        &mut self,
        start: NonNull<T>,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<T>
    where
        T: Send + Sync + 'static,
    {
        if !UNIQUE && !self.is_unique() {
            return (Err(TryReserveError::NotUnique), start);
        }
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => {
                let (capacity, start) = unsafe {
                    let try_reserve = vtable.try_reserve.unwrap_unchecked();
                    try_reserve(
                        self.inner.cast(),
                        start.cast(),
                        length,
                        additional,
                        allocate,
                    )
                };
                (capacity, start.cast())
            }
            VTableOrCapacity::Capacity(_) => {
                let offset = unsafe { start.sub_ptr(self.slice_start()) };
                if let Some(slice_length) = unsafe { self.slice_length() } {
                    if offset + length != slice_length {
                        return (Err(TryReserveError::Unsupported), start);
                    }
                }
                struct ArcSlice<T> {
                    arc: ManuallyDrop<Arc<T, false>>,
                    length: usize,
                }
                impl<T: Send + Sync + 'static> Buffer<T> for ArcSlice<T> {
                    fn as_slice(&self) -> &[T] {
                        unsafe {
                            slice::from_raw_parts(self.arc.slice_start().as_ptr(), self.length)
                        }
                    }
                }
                unsafe impl<T: Send + Sync + 'static> BufferMut<T> for ArcSlice<T> {
                    fn as_mut_ptr(&mut self) -> NonNull<T> {
                        unsafe { self.arc.slice_start() }
                    }
                    fn len(&self) -> usize {
                        self.length
                    }
                    fn capacity(&self) -> usize {
                        match self.arc.vtable_or_capacity() {
                            VTableOrCapacity::Capacity(capacity) => capacity,
                            VTableOrCapacity::VTable(_) => unreachable!(),
                        }
                    }
                    unsafe fn set_len(&mut self, len: usize) -> bool {
                        self.length = len;
                        true
                    }
                    fn reserve(&mut self, additional: usize) -> bool {
                        let (start, capacity) = unsafe {
                            self.realloc(additional, self.arc.inner.cast(), Arc::<T>::slice_layout)
                        };
                        self.arc.inner = start.cast();
                        unsafe { self.arc.inner.as_mut() }.vtable_or_capacity =
                            ptr::without_provenance(capacity);
                        true
                    }
                }
                let mut buffer = ArcSlice {
                    arc: ManuallyDrop::new(Arc {
                        inner: self.inner,
                        _phantom: self._phantom,
                    }),
                    length: offset + length,
                };
                let res = unsafe { buffer.try_reserve_impl(offset, length, additional, allocate) };
                self.inner = buffer.arc.inner;
                res
            }
        }
    }

    unsafe fn deallocate(&mut self) {
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => unsafe {
                (vtable.deallocate)(self.as_ptr().cast_mut());
            },
            VTableOrCapacity::Capacity(capacity) => {
                if mem::needs_drop::<T>() {
                    unsafe {
                        ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                            self.slice_start().as_ptr(),
                            self.slice_length().unwrap_checked(),
                        ));
                    };
                }
                let layout = unsafe { Self::slice_layout(capacity).unwrap_unchecked() };
                unsafe { dealloc(self.inner.as_ptr().cast(), layout) };
            }
        }
    }

    pub(crate) unsafe fn drop_unique(self) {
        unsafe { ManuallyDrop::new(self).deallocate() };
    }

    pub(crate) fn drop_with_unique_hint<const UNIQUE_HINT: bool>(mut self) {
        if UNIQUE_HINT && self.is_unique() {
            unsafe { self.drop_unique() };
        } else {
            drop(self);
        }
    }
}

impl<T> Arc<T> {
    fn allocate_buffer<B>(refcount: usize, vtable: &'static VTable, buffer: B) -> Box<ArcInner<B>> {
        let vtable_ptr = ptr::from_ref(vtable);
        Box::new(ArcInner {
            refcount: AtomicUsize::new(refcount),
            vtable_or_capacity: vtable_ptr
                .with_addr(VTABLE_FLAG | (vtable_ptr.addr() >> VTABLE_SHIFT))
                .cast(),
            buffer,
        })
    }

    #[allow(unstable_name_collisions)]
    fn new_guard<B>(vtable: &'static VTable, buffer: B) -> ArcGuard<B> {
        ArcGuard(Box::into_non_null(Self::allocate_buffer(1, vtable, buffer)))
    }

    pub(crate) fn new_vec(vec: Vec<T>) -> Self
    where
        T: Send + Sync + 'static,
    {
        if mem::needs_drop::<T>() {
            Self::new_guard(vtable::new::<T, FullVec<T>>(), FullVec::new(vec, ())).into()
        } else {
            Self::new_guard(vtable::new_compact_vec::<T>(), CompactVec::from(vec)).into()
        }
    }

    pub(crate) fn new_buffer<B: DynBuffer + Buffer<T>>(buffer: B) -> (Self, NonNull<T>, usize) {
        let arc = Self::new_guard(vtable::new::<T, B>(), buffer);
        let (start, length) = slice_into_raw_parts(arc.buffer().as_slice());
        (arc.into(), start, length)
    }

    pub(crate) fn new_buffer_mut<B: DynBuffer + BufferMut<T>>(
        buffer: B,
    ) -> (Self, NonNull<T>, usize, usize) {
        let mut arc = Self::new_guard(vtable::new_mut::<T, B>(), buffer);
        let buffer = arc.buffer_mut();
        let start = buffer.as_mut_ptr();
        let length = buffer.len();
        let capacity = buffer.capacity();
        (arc.into(), start, length, capacity)
    }

    #[allow(unstable_name_collisions)]
    pub(crate) fn promote_vec(vec: Vec<T>) -> PromoteGuard<T>
    where
        T: Send + Sync + 'static,
    {
        fn guard<T, B>(vtable: &'static VTable, buffer: B) -> PromoteGuard<T> {
            let arc = Arc::<T, true>::allocate_buffer(2, vtable, buffer);
            PromoteGuard {
                arc: Box::into_non_null(arc).cast(),
                _phantom: PhantomData,
            }
        }
        if mem::needs_drop::<T>() {
            guard(vtable::new::<T, FullVec<T>>(), FullVec::new(vec, ()))
        } else {
            guard(vtable::new_compact_vec::<T>(), CompactVec::from(vec))
        }
    }
}

unsafe impl<T: Send + Sync, const ANY_BUFFER: bool> Send for Arc<T, ANY_BUFFER> {}
unsafe impl<T: Send + Sync, const ANY_BUFFER: bool> Sync for Arc<T, ANY_BUFFER> {}

impl<T, const ANY_BUFFER: bool> Drop for Arc<T, ANY_BUFFER> {
    fn drop(&mut self) {
        if unsafe { self.inner.as_ref() }.decr_refcount() {
            unsafe { self.deallocate() };
        }
    }
}

impl<T, const ANY_BUFFER: bool> Clone for Arc<T, ANY_BUFFER> {
    fn clone(&self) -> Self {
        unsafe { self.inner.as_ref() }.incr_refcount();
        Self {
            inner: self.inner,
            _phantom: PhantomData,
        }
    }
}

struct ArcGuard<B>(NonNull<ArcInner<B>>);

impl<B> ArcGuard<B> {
    fn buffer(&self) -> &B {
        &unsafe { self.0.as_ref() }.buffer
    }

    fn buffer_mut(&mut self) -> &mut B {
        &mut unsafe { self.0.as_mut() }.buffer
    }
}

#[allow(unstable_name_collisions)]
impl<B> Drop for ArcGuard<B> {
    fn drop(&mut self) {
        drop(unsafe { Box::from_non_null(self.0) });
    }
}

impl<T, B> From<ArcGuard<B>> for Arc<T> {
    fn from(value: ArcGuard<B>) -> Self {
        let guard = ManuallyDrop::new(value);
        Self {
            inner: guard.0.cast(),
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
        let ptr = self.arc.as_ptr();
        if mem::needs_drop::<T>() {
            drop(unsafe { Box::from_raw(ptr.cast::<ArcInner<MaybeUninit<FullVec<T>>>>()) });
        } else {
            drop(unsafe { Box::from_raw(ptr.cast::<ArcInner<MaybeUninit<CompactVec<T>>>>()) });
        }
    }
}

impl<T> From<PromoteGuard<T>> for Arc<T> {
    fn from(value: PromoteGuard<T>) -> Self {
        unsafe { Self::from_raw(ManuallyDrop::new(value).arc) }
    }
}
