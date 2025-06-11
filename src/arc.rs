use alloc::{alloc::dealloc, boxed::Box, vec::Vec};
use core::{
    alloc::{Layout, LayoutError},
    any::{Any, TypeId},
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::{addr_of_mut, NonNull},
    sync::atomic::Ordering,
};

#[allow(unused_imports)]
use crate::msrv::{BoxExt, ConstPtrExt, NonNullExt, OffsetFromUnsignedExt, StrictProvenance};
use crate::{
    atomic,
    atomic::AtomicUsize,
    buffer::{
        Buffer, BufferExt, BufferMut, BufferMutExt, BufferWithMetadata, DynBuffer, Slice, SliceExt,
    },
    error::{AllocErrorImpl, TryReserveError},
    macros::is,
    msrv::{ptr, NonZero},
    slice_mut::TryReserveResult,
    utils::{assert_checked, unreachable_checked, NewChecked, UnwrapChecked},
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

struct CompactVec<S: Slice + ?Sized> {
    start: NonNull<S::Item>,
    capacity: NonZero<usize>,
}

impl<S: Slice + ?Sized> CompactVec<S> {
    fn new(value: S::Vec) -> Self {
        assert_checked(!S::needs_drop());

        let mut vec = ManuallyDrop::new(value);
        CompactVec {
            start: S::vec_start(&mut vec),
            capacity: unsafe { NonZero::new_unchecked(vec.capacity()) },
        }
    }

    unsafe fn to_vec(&self, length: usize) -> S::Vec {
        let start = self.start.as_ptr();
        let capacity = self.capacity.get();
        unsafe { S::from_vec_unchecked(Vec::from_raw_parts(start, length, capacity)) }
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
    ) -> Option<NonNull<()>> {
        let inner = unsafe { vtable::check_unique::<Self>(ptr)? };
        let vec = &unsafe { &*inner }.buffer;
        let capacity = vec.capacity.get();
        if is!({ type_id }, S::Vec) {
            if start.cast::<S::Item>() != vec.start {
                let start = start.cast::<S::Item>().as_ptr();
                unsafe { ptr::copy(start, vec.start.as_ptr(), length) };
            }
            unsafe { buffer.cast().write(vec.to_vec(length)) };
        } else if is!({ type_id }, Box<S>) && length == capacity {
            let slice = ptr::slice_from_raw_parts_mut(vec.start.as_ptr(), capacity);
            unsafe { buffer.cast().write(Box::from_raw(slice)) };
        } else {
            return None;
        }
        drop(unsafe { Box::from_raw(inner.cast::<ArcInner<MaybeUninit<Self>>>()) });
        Some(buffer)
    }

    unsafe fn capacity(ptr: *const (), start: NonNull<()>) -> usize {
        // MSRV 1.65 let-else
        let buffer = match unsafe { vtable::check_unique::<Self>(ptr) } {
            Some(inner) => &unsafe { &*inner }.buffer,
            None => return usize::MAX,
        };
        let offset = unsafe { start.cast().offset_from_unsigned(buffer.start) };
        buffer.capacity.get() - offset
    }

    #[allow(unstable_name_collisions)]
    unsafe fn try_reserve(
        ptr: NonNull<()>,
        start: NonNull<()>,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<()> {
        struct ArcCompactVec<S: Slice + ?Sized> {
            arc: ManuallyDrop<Box<ArcInner<CompactVec<S>>>>,
            length: usize,
        }
        unsafe impl<S: Slice + ?Sized> Send for ArcCompactVec<S> {}
        unsafe impl<S: Slice + ?Sized> Sync for ArcCompactVec<S> {}
        impl<S: Slice + ?Sized> Buffer<S> for ArcCompactVec<S> {
            fn as_slice(&self) -> &S {
                unsafe { S::from_raw_parts(self.arc.buffer.start, self.length) }
            }
        }
        unsafe impl<S: Slice + ?Sized> BufferMut<S> for ArcCompactVec<S> {
            fn as_mut_slice(&mut self) -> &mut S {
                unsafe { S::from_raw_parts_mut(self.arc.buffer.start, self.length) }
            }
            fn capacity(&self) -> usize {
                self.arc.buffer.capacity.get()
            }
            unsafe fn set_len(&mut self, len: usize) -> bool {
                self.length = len;
                true
            }
            fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
                let (start, capacity) = unsafe {
                    self.realloc(additional, self.arc.buffer.start, Layout::array::<S::Item>)?
                };
                self.arc.buffer.start = start;
                self.arc.buffer.capacity = unsafe { NonZero::new_unchecked(capacity) };
                Ok(())
            }
        }
        let arc = ManuallyDrop::new(unsafe { Box::from_non_null(ptr.cast::<ArcInner<Self>>()) });
        let offset = unsafe { start.cast().offset_from_unsigned(arc.buffer.start) };
        let mut buffer = ArcCompactVec {
            arc,
            length: offset + length,
        };
        let (capacity, start) = unsafe {
            buffer.try_reserve_impl(
                offset,
                length,
                additional,
                allocate,
                |vec| vec.arc.buffer.start,
                || (),
            )
        };
        (capacity, start.cast())
    }
}

impl<S: Slice + ?Sized> Drop for CompactVec<S> {
    fn drop(&mut self) {
        drop(unsafe { self.to_vec(0) });
    }
}

#[allow(type_alias_bounds)]
type FullVec<S: Slice + ?Sized> = BufferWithMetadata<S::Vec, ()>;

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
        buffer::{Buffer, BufferExt, BufferMut, BufferMutExt, DynBuffer, Slice, SliceExt},
        error::TryReserveError,
        macros::{is, is_not},
        slice_mut::TryReserveResult,
        vtable::{no_capacity, VTable},
    };

    unsafe fn deallocate<B>(ptr: *mut ()) {
        mem::drop(unsafe { Box::from_raw(ptr.cast::<ArcInner<B>>()) });
    }
    unsafe fn is_buffer_unique<S: ?Sized, B: Buffer<S>>(ptr: *const ()) -> bool {
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

    unsafe fn take_buffer<S: Slice + ?Sized, B: DynBuffer + Buffer<S>>(
        buffer: NonNull<()>,
        ptr: *const (),
        type_id: TypeId,
        _start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>> {
        let inner = unsafe { check_unique::<B>(ptr)? };
        if is_not!({ type_id }, B::Buffer) || unsafe { &*inner }.buffer.len() != length {
            return None;
        }
        unsafe { B::take_buffer(addr_of_mut!((*inner).buffer), buffer) };
        mem::drop(unsafe { Box::from_raw(inner.cast::<ArcInner<MaybeUninit<B>>>()) });
        Some(buffer)
    }

    unsafe fn capacity<S: Slice + ?Sized, B: BufferMut<S>>(
        ptr: *const (),
        start: NonNull<()>,
    ) -> usize {
        let buffer = match unsafe { check_unique::<B>(ptr) } {
            Some(inner) => &unsafe { &*inner }.buffer,
            None => return usize::MAX,
        };
        buffer.capacity() - unsafe { buffer.offset(start.cast()) }
    }

    unsafe fn try_reserve<S: Slice + ?Sized, B: BufferMut<S>>(
        ptr: NonNull<()>,
        start: NonNull<()>,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<()> {
        let buffer = &mut unsafe { ptr.cast::<ArcInner<B>>().as_mut() }.buffer;
        let offset = unsafe { buffer.offset(start.cast()) };
        if S::needs_drop() && buffer.len() != offset + length {
            return (Err(TryReserveError::Unsupported), start);
        }
        let (capacity, start) = unsafe {
            buffer.try_reserve_impl(
                offset,
                length,
                additional,
                allocate,
                |b| b.as_mut_slice().as_mut_ptr(),
                || (),
            )
        };
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

    #[cfg(feature = "raw-buffer")]
    unsafe fn into_arc_fallible(
        ptr: *const (),
    ) -> Result<Option<NonNull<()>>, crate::error::AllocError> {
        Ok(NonNull::new(ptr.cast_mut()))
    }

    pub(crate) fn new<S: ?Sized + Slice, B: DynBuffer + Buffer<S>>() -> &'static VTable {
        &VTable {
            deallocate: deallocate::<B>,
            is_buffer_unique: is_buffer_unique::<S, B>,
            get_metadata: get_metadata::<B>,
            take_buffer: take_buffer::<S, B>,
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
            #[cfg(feature = "raw-buffer")]
            into_arc_fallible,
        }
    }

    pub(crate) fn new_mut<S: ?Sized + Slice, B: DynBuffer + BufferMut<S>>() -> &'static VTable {
        &VTable {
            deallocate: deallocate::<B>,
            is_buffer_unique: is_buffer_unique::<S, B>,
            get_metadata: get_metadata::<B>,
            take_buffer: take_buffer::<S, B>,
            capacity: capacity::<S, B>,
            try_reserve: Some(try_reserve::<S, B>),
            #[cfg(feature = "raw-buffer")]
            drop: drop::<B>,
            #[cfg(feature = "raw-buffer")]
            drop_with_unique_hint: drop_with_unique_hint::<B>,
            #[cfg(feature = "raw-buffer")]
            clone,
            #[cfg(feature = "raw-buffer")]
            into_arc,
            #[cfg(feature = "raw-buffer")]
            into_arc_fallible,
        }
    }

    pub(crate) fn new_vec<S: Slice + ?Sized>() -> &'static VTable {
        if S::needs_drop() {
            new::<S, super::FullVec<S>>()
        } else {
            &VTable {
                deallocate: deallocate::<CompactVec<S>>,
                is_buffer_unique: CompactVec::<S>::is_buffer_unique,
                get_metadata: CompactVec::<S>::get_metadata,
                take_buffer: CompactVec::<S>::take_buffer,
                capacity: CompactVec::<S>::capacity,
                try_reserve: Some(CompactVec::<S>::try_reserve),
                #[cfg(feature = "raw-buffer")]
                drop: drop::<CompactVec<S>>,
                #[cfg(feature = "raw-buffer")]
                drop_with_unique_hint: drop_with_unique_hint::<CompactVec<S>>,
                #[cfg(feature = "raw-buffer")]
                clone,
                #[cfg(feature = "raw-buffer")]
                into_arc,
                #[cfg(feature = "raw-buffer")]
                into_arc_fallible,
            }
        }
    }
}

enum VTableOrCapacity {
    VTable(&'static VTable),
    Capacity(usize),
}

#[allow(missing_debug_implementations)]
pub struct Arc<S: Slice + ?Sized, const ANY_BUFFER: bool = true> {
    inner: ErasedArc,
    _phantom: PhantomData<S>,
}

unsafe impl<S: Slice + ?Sized, const ANY_BUFFER: bool> Send for Arc<S, ANY_BUFFER> {}
unsafe impl<S: Slice + ?Sized, const ANY_BUFFER: bool> Sync for Arc<S, ANY_BUFFER> {}

impl<S: Slice + ?Sized, const ANY_BUFFER: bool> Arc<S, ANY_BUFFER> {
    fn slice_layout(capacity: usize) -> Result<Layout, LayoutError> {
        let inner_layout = if S::needs_drop() {
            Layout::new::<ArcInner<WithLength<[S::Item; 0]>>>()
        } else {
            Layout::new::<ArcInner<[S::Item; 0]>>()
        };
        let (layout, _) = inner_layout.extend(Layout::array::<S::Item>(capacity)?)?;
        Ok(layout)
    }

    unsafe fn slice_start(&self) -> NonNull<S::Item> {
        NonNull::new_checked(if S::needs_drop() {
            let inner = self.inner.cast::<ArcInner<WithLength<[S::Item; 0]>>>();
            unsafe { addr_of_mut!((*inner.as_ptr()).buffer.buffer) }
        } else {
            let inner = self.inner.cast::<ArcInner<[S::Item; 0]>>();
            unsafe { addr_of_mut!((*inner.as_ptr()).buffer) }
        })
        .cast()
    }

    unsafe fn slice_length(&self) -> Option<usize> {
        if S::needs_drop() {
            let inner = self.inner.cast::<ArcInner<WithLength<[S::Item; 0]>>>();
            Some((unsafe { inner.as_ref() }).buffer.length)
        } else {
            None
        }
    }

    unsafe fn set_length_unchecked(&mut self, length: usize) {
        assert_checked(S::needs_drop());
        let inner = self.inner.cast::<ArcInner<WithLength<[S::Item; 0]>>>();
        unsafe { addr_of_mut!((*inner.as_ptr()).buffer.length).write(length) };
    }

    fn allocate_slice<E: AllocErrorImpl, const ZEROED: bool>(
        capacity: usize,
        length: usize,
    ) -> Result<(Self, NonNull<S::Item>), E> {
        let layout = Self::slice_layout(capacity).map_err(|_| E::capacity_overflow())?;
        let inner_ptr = E::alloc::<_, ZEROED>(layout)?;
        let inner = ArcInner {
            refcount: AtomicUsize::new(1),
            vtable_or_capacity: ptr::without_provenance(capacity),
            buffer: (),
        };
        unsafe { inner_ptr.write(inner) };
        let mut arc = Self {
            inner: inner_ptr.cast(),
            _phantom: PhantomData,
        };
        if S::needs_drop() {
            unsafe { arc.set_length_unchecked(length) };
        }
        let start = unsafe { arc.slice_start() };
        Ok((arc, start))
    }

    pub(crate) fn with_capacity<E: AllocErrorImpl, const ZEROED: bool>(
        capacity: usize,
    ) -> Result<(Self, NonNull<S::Item>), E> {
        Self::allocate_slice::<E, ZEROED>(capacity, 0)
    }

    pub(crate) unsafe fn new_unchecked<E: AllocErrorImpl>(
        slice: &[S::Item],
    ) -> Result<(Self, NonNull<S::Item>), E> {
        let (arc, start) = Self::allocate_slice::<E, false>(slice.len(), slice.len())?;
        unsafe { ptr::copy_nonoverlapping(slice.as_ptr(), start.as_ptr(), slice.len()) };
        Ok((arc, start))
    }

    pub(crate) fn new<E: AllocErrorImpl>(slice: &S) -> Result<(Self, NonNull<S::Item>), E>
    where
        S::Item: Copy,
    {
        unsafe { Self::new_unchecked(slice.to_slice()) }
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn new_array<E: AllocErrorImpl, const N: usize>(
        array: [S::Item; N],
    ) -> Result<(Self, NonNull<S::Item>), (E, [S::Item; N])> {
        let array = ManuallyDrop::new(array);
        unsafe { Self::new_unchecked::<E>(&array[..]) }
            .map_err(|err| (err, ManuallyDrop::into_inner(array)))
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

    pub(crate) fn try_into_arc_slice(self) -> Result<Arc<S, false>, Self> {
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(_) => Err(self),
            VTableOrCapacity::Capacity(_) => Ok(unsafe { Arc::from_raw(self.into_raw()) }),
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

    pub(crate) unsafe fn take_buffer<B: Buffer<S>, const UNIQUE: bool>(
        self,
        start: NonNull<S::Item>,
        length: usize,
    ) -> Result<B, Self> {
        let this = ManuallyDrop::new(self);
        if let VTableOrCapacity::VTable(vtable) = this.vtable_or_capacity() {
            if let Some(buffer) =
                unsafe { generic_take_buffer::<B>(this.as_ptr(), vtable, start.cast(), length) }
            {
                return Ok(buffer);
            }
        }
        Err(ManuallyDrop::into_inner(this))
    }

    pub(crate) unsafe fn take_array<const N: usize, const UNIQUE: bool>(
        self,
        start: NonNull<S::Item>,
        length: usize,
    ) -> Result<[S::Item; N], Self> {
        let mut this = ManuallyDrop::new(self);
        match this.vtable_or_capacity() {
            VTableOrCapacity::Capacity(capacity)
                if (UNIQUE || this.is_unique()) && length == capacity =>
            {
                let mut array = MaybeUninit::<[S::Item; N]>::uninit();
                unsafe {
                    ptr::copy_nonoverlapping(start.as_ptr(), array.as_mut_ptr().cast(), capacity);
                }
                let layout = unsafe { Self::slice_layout(capacity).unwrap_unchecked() };
                unsafe { dealloc(this.inner.as_ptr().cast(), layout) };
                Ok(unsafe { array.assume_init() })
            }
            _ => Err(ManuallyDrop::into_inner(this)),
        }
    }

    pub(crate) unsafe fn capacity(&mut self, start: NonNull<S::Item>) -> Option<usize> {
        match self.vtable_or_capacity() {
            VTableOrCapacity::VTable(vtable) => {
                Some(unsafe { (vtable.capacity)(self.as_ptr(), start.cast()) })
                    .filter(|&capacity| capacity != usize::MAX)
            }
            VTableOrCapacity::Capacity(capacity) => self
                .is_unique()
                .then(|| capacity - unsafe { start.offset_from_unsigned(self.slice_start()) }),
        }
    }

    pub(crate) unsafe fn try_reserve<const UNIQUE: bool>(
        &mut self,
        start: NonNull<S::Item>,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<S::Item> {
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
                let offset = unsafe { start.offset_from_unsigned(self.slice_start()) };
                if let Some(slice_length) = unsafe { self.slice_length() } {
                    if offset + length != slice_length {
                        return (Err(TryReserveError::Unsupported), start);
                    }
                }
                struct ArcSliceBuffer<S: Slice + ?Sized> {
                    arc: ManuallyDrop<Arc<S, false>>,
                    length: usize,
                }
                impl<S: Slice + ?Sized> Buffer<S> for ArcSliceBuffer<S> {
                    fn as_slice(&self) -> &S {
                        unsafe { S::from_raw_parts(self.arc.slice_start(), self.length) }
                    }
                }
                unsafe impl<S: Slice + ?Sized> BufferMut<S> for ArcSliceBuffer<S> {
                    fn as_mut_slice(&mut self) -> &mut S {
                        unsafe { S::from_raw_parts_mut(self.arc.slice_start(), self.length) }
                    }
                    fn capacity(&self) -> usize {
                        match self.arc.vtable_or_capacity() {
                            VTableOrCapacity::Capacity(capacity) => capacity,
                            VTableOrCapacity::VTable(_) => unreachable_checked(),
                        }
                    }
                    unsafe fn set_len(&mut self, len: usize) -> bool {
                        self.length = len;
                        true
                    }
                    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
                        let (inner, capacity) = unsafe {
                            self.realloc(additional, self.arc.inner, Arc::<S>::slice_layout)?
                        };
                        self.arc.inner = inner;
                        unsafe { self.arc.inner.as_mut() }.vtable_or_capacity =
                            ptr::without_provenance(capacity);
                        Ok(())
                    }
                }
                let mut buffer = ArcSliceBuffer {
                    arc: ManuallyDrop::new(Arc {
                        inner: self.inner,
                        _phantom: self._phantom,
                    }),
                    length: offset + length,
                };
                let res = unsafe {
                    buffer.try_reserve_impl(
                        offset,
                        length,
                        additional,
                        allocate,
                        |arc| arc.arc.slice_start(),
                        || (),
                    )
                };
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
                if S::needs_drop() {
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

    pub(crate) fn set_length<const UNIQUE: bool>(
        &mut self,
        start: NonNull<S::Item>,
        length: usize,
    ) {
        if S::needs_drop() && (UNIQUE || self.is_unique()) {
            let offset = unsafe { start.offset_from_unsigned(self.slice_start()) };
            unsafe { self.set_length_unchecked(offset + length) };
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

impl<S: Slice + ?Sized> Arc<S> {
    #[allow(unstable_name_collisions)]
    fn allocate_buffer<B, E: AllocErrorImpl>(
        refcount: usize,
        vtable: &'static VTable,
        buffer: B,
    ) -> Result<Box<ArcInner<B>>, (E, B)> {
        let vtable_ptr = ptr::from_ref(vtable);
        let layout = Layout::new::<ArcInner<B>>();
        // MSRV 1.65 let-else
        let ptr = match E::alloc::<_, true>(layout) {
            Ok(ptr) => ptr,
            Err(err) => return Err((err, buffer)),
        };
        let inner = ArcInner {
            refcount: AtomicUsize::new(refcount),
            vtable_or_capacity: vtable_ptr
                .with_addr(VTABLE_FLAG | (vtable_ptr.addr() >> VTABLE_SHIFT))
                .cast(),
            buffer,
        };
        unsafe { ptr.write(inner) }
        Ok(unsafe { Box::from_non_null(ptr) })
    }

    #[allow(unstable_name_collisions)]
    fn new_guard<B, E: AllocErrorImpl>(
        vtable: &'static VTable,
        buffer: B,
    ) -> Result<ArcGuard<B>, (E, B)> {
        Ok(ArcGuard(Box::into_non_null(Self::allocate_buffer::<_, E>(
            1, vtable, buffer,
        )?)))
    }

    pub(crate) fn new_vec<E: AllocErrorImpl>(vec: S::Vec) -> Result<Self, (E, S::Vec)> {
        if S::needs_drop() {
            let guard = Self::new_guard::<_, E>(vtable::new_vec::<S>(), FullVec::<S>::new(vec, ()))
                .map_err(|(err, b)| (err, b.buffer()))?;
            Ok(guard.into())
        } else {
            let len = vec.len();
            let guard = Self::new_guard::<_, E>(vtable::new_vec::<S>(), CompactVec::<S>::new(vec))
                .map_err(|(err, b)| (err, unsafe { b.to_vec(len) }))?;
            Ok(guard.into())
        }
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn new_buffer<B: DynBuffer + Buffer<S>, E: AllocErrorImpl>(
        buffer: B,
    ) -> Result<(Self, NonNull<S::Item>, usize), (E, B)> {
        let arc = Self::new_guard::<_, E>(vtable::new::<S, B>(), buffer)?;
        let (start, length) = arc.buffer().as_slice().to_raw_parts();
        Ok((arc.into(), start, length))
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn new_buffer_mut<B: DynBuffer + BufferMut<S>, E: AllocErrorImpl>(
        buffer: B,
    ) -> Result<(Self, NonNull<S::Item>, usize, usize), (E, B)> {
        let mut arc = Self::new_guard::<_, E>(vtable::new_mut::<S, B>(), buffer)?;
        let (start, length) = arc.buffer_mut().as_mut_slice().to_raw_parts_mut();
        let capacity = arc.buffer_mut().capacity();
        Ok((arc.into(), start, length, capacity))
    }

    #[allow(unstable_name_collisions)]
    pub(crate) fn promote_vec<E: AllocErrorImpl>(vec: S::Vec) -> Result<PromoteGuard<S>, E>
where {
        fn guard<S: Slice + ?Sized, B, E: AllocErrorImpl>(
            vtable: &'static VTable,
            buffer: B,
        ) -> Result<PromoteGuard<S>, E> {
            let arc = Arc::<S, true>::allocate_buffer::<_, E>(2, vtable, buffer)
                .map_err(|(err, b)| err.forget(b))?;
            Ok(PromoteGuard {
                arc: Box::into_non_null(arc).cast(),
                _phantom: PhantomData,
            })
        }
        if S::needs_drop() {
            guard::<_, _, E>(vtable::new_vec::<S>(), FullVec::<S>::new(vec, ()))
        } else {
            guard::<_, _, E>(vtable::new_vec::<S>(), CompactVec::<S>::new(vec))
        }
    }
}

impl<S: Slice + ?Sized, const ANY_BUFFER: bool> Drop for Arc<S, ANY_BUFFER> {
    fn drop(&mut self) {
        if unsafe { self.inner.as_ref() }.decr_refcount() {
            unsafe { self.deallocate() };
        }
    }
}

impl<S: Slice + ?Sized, const ANY_BUFFER: bool> Clone for Arc<S, ANY_BUFFER> {
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

impl<S: Slice + ?Sized, B> From<ArcGuard<B>> for Arc<S> {
    fn from(value: ArcGuard<B>) -> Self {
        let guard = ManuallyDrop::new(value);
        Self {
            inner: guard.0.cast(),
            _phantom: PhantomData,
        }
    }
}

pub(crate) struct PromoteGuard<S: Slice + ?Sized> {
    arc: NonNull<()>,
    _phantom: PhantomData<S>,
}

impl<S: Slice + ?Sized> PromoteGuard<S> {
    pub(crate) fn as_ptr(&self) -> *mut () {
        self.arc.as_ptr()
    }
}

impl<S: Slice + ?Sized> Drop for PromoteGuard<S> {
    fn drop(&mut self) {
        let ptr = self.arc.as_ptr();
        if S::needs_drop() {
            drop(unsafe { Box::from_raw(ptr.cast::<ArcInner<MaybeUninit<FullVec<S>>>>()) });
        } else {
            drop(unsafe { Box::from_raw(ptr.cast::<ArcInner<MaybeUninit<CompactVec<S>>>>()) });
        }
    }
}

impl<S: Slice + ?Sized> From<PromoteGuard<S>> for Arc<S> {
    fn from(value: PromoteGuard<S>) -> Self {
        unsafe { Self::from_raw(ManuallyDrop::new(value).arc) }
    }
}
