use alloc::{boxed::Box, vec::Vec};
use core::{
    self,
    any::{Any, TypeId},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::{addr_of, addr_of_mut, NonNull},
    sync::atomic,
};

#[allow(unused_imports)]
use crate::msrv::NonNullExt;
use crate::{
    buffer::{BorrowMetadata, Buffer, BufferMut, BufferMutExt},
    error::TryReserveError,
    loom::{
        atomic_usize_with_mut,
        sync::atomic::{AtomicUsize, Ordering},
    },
    macros::{is, is_not},
    msrv::{ptr, BoxExt, NonZero, SubPtrExt},
    ArcSliceMut,
};

pub(crate) fn unit_metadata<M: Any>() -> &'static M {
    assert_eq!(TypeId::of::<M>(), TypeId::of::<()>());
    unsafe { NonNull::dangling().as_ref() }
}

// The structure needs to be repr(C) to allow pointer casting between `Arc` and
// `ArcInner<R, B, M>`. `align(4)` is added to ensure the possibility of pointer tagging.
#[repr(C, align(4))]
struct ArcInner<C, B, M> {
    rc: AtomicUsize,
    vtable_or_capa: VTableOrCapacity,
    spare_capacity: C,
    buffer: B,
    metadata: M,
}

type ErasedArc = NonNull<ArcInner<(), (), ()>>;
type TryReserveResult<T> = (Result<usize, TryReserveError>, NonNull<T>);

#[repr(align(2))]
struct VTable {
    dealloc: unsafe fn(ErasedArc),
    #[allow(clippy::type_complexity)]
    get_metadata: Option<unsafe fn(ErasedArc, TypeId) -> Option<NonNull<()>>>,
    take_buffer: unsafe fn(ErasedArc, TypeId, usize, NonNull<()>) -> bool,
    into_mut: Option<unsafe fn(ErasedArc, NonNull<()>)>,
    #[allow(clippy::type_complexity)]
    try_reserve:
        Option<unsafe fn(ErasedArc, usize, bool, NonNull<()>, usize) -> TryReserveResult<()>>,
}

impl VTable {
    unsafe fn update_capacity<T: Send + Sync + 'static, C: 'static, B: BufferMut<T>>(
        arc: ErasedArc,
    ) {
        if mem::needs_drop::<T>() {
            assert!(is!(C, AtomicUsize));
            let inner = unsafe { arc.cast::<ArcInner<AtomicUsize, B, ()>>().as_mut() };
            let spare_capacity = atomic_usize_with_mut(&mut inner.spare_capacity, |c| *c);
            if spare_capacity != usize::MAX {
                let len = inner.buffer.capacity() - spare_capacity;
                unsafe { inner.buffer.set_len(len) };
            }
        }
    }

    unsafe fn dealloc<C, B, M>(arc: ErasedArc) {
        drop(unsafe { Box::from_raw(arc.cast::<ArcInner<C, B, M>>().as_ptr()) });
    }

    unsafe fn dealloc_mut<T: Send + Sync + 'static, C: 'static, B: BufferMut<T>, M>(
        arc: ErasedArc,
    ) {
        unsafe { Self::update_capacity::<T, C, B>(arc) }
        unsafe { Self::dealloc::<C, B, M>(arc) }
    }

    unsafe fn get_metadata<C, B, M: Any>(arc: ErasedArc, type_id: TypeId) -> Option<NonNull<()>> {
        if is_not!({ type_id }, M) {
            return None;
        }
        if is!({ type_id }, ()) {
            return Some(NonNull::from(unit_metadata()));
        }
        Some(NonNull::from(&unsafe { arc.cast::<ArcInner<C, B, M>>().as_ref() }.metadata).cast())
    }

    unsafe fn get_borrowed_metadata<C, B: BorrowMetadata>(
        arc: ErasedArc,
        type_id: TypeId,
    ) -> Option<NonNull<()>> {
        if is_not!({ type_id }, B::Metadata) {
            return None;
        }
        let inner = unsafe { arc.cast::<ArcInner<C, B, ()>>().as_ref() };
        Some(NonNull::from(inner.buffer.borrow_metadata()).cast())
    }

    unsafe fn take_buffer<T: 'static, C, B: Any, M>(
        arc: ErasedArc,
        type_id: TypeId,
        len: usize,
        buffer_ptr: NonNull<()>,
        buffer_len: impl FnOnce(&B) -> usize,
    ) -> bool {
        if is_not!({ type_id }, B) {
            return false;
        }
        let inner = unsafe { arc.cast::<ArcInner<C, B, M>>().as_mut() };
        if is_not!(B, Vec<T>) && len != buffer_len(&mut inner.buffer) {
            return false;
        }
        let inner_ptr = ptr::from_mut(inner);
        let buffer_src = unsafe { addr_of!((*inner_ptr).buffer) };
        let buffer_dst = buffer_ptr.cast::<B>().as_ptr();
        unsafe { ptr::copy_nonoverlapping(buffer_src, buffer_dst, 1) }
        unsafe { ptr::drop_in_place(addr_of_mut!((*inner_ptr).metadata)) };
        drop(unsafe { Box::from_raw(inner_ptr.cast::<MaybeUninit<ArcInner<C, B, M>>>()) });
        true
    }

    unsafe fn take_buffer_const<T: Send + Sync + 'static, B: Buffer<T>, M>(
        arc: ErasedArc,
        type_id: TypeId,
        len: usize,
        buffer_ptr: NonNull<()>,
    ) -> bool {
        let buffer_len = |buffer: &B| buffer.as_slice().len();
        unsafe { Self::take_buffer::<T, (), B, M>(arc, type_id, len, buffer_ptr, buffer_len) }
    }

    unsafe fn take_buffer_mut<T: Send + Sync + 'static, C: 'static, B: BufferMut<T>, M>(
        arc: ErasedArc,
        type_id: TypeId,
        len: usize,
        buffer_ptr: NonNull<()>,
    ) -> bool {
        let buffer_len = |buffer: &B| buffer.len();
        unsafe { Self::update_capacity::<T, C, B>(arc) };
        unsafe { Self::take_buffer::<T, C, B, M>(arc, type_id, len, buffer_ptr, buffer_len) }
    }

    #[allow(clippy::incompatible_msrv)]
    unsafe fn into_mut<T: Send + Sync + 'static, C, B: BufferMut<T>>(
        arc: ErasedArc,
        slice_mut_ptr: NonNull<()>,
    ) {
        let inner = unsafe { arc.cast::<ArcInner<C, B, ()>>().as_mut() };
        // execute `BufferMut` method before instantiating the `Arc` in case of panic,
        // so the `Arc` will not be dropped
        let slice_mut = ArcSliceMut::from_arc(
            inner.buffer.as_mut_ptr(),
            inner.buffer.len(),
            inner.buffer.capacity(),
            arc.into(),
        );
        unsafe { slice_mut_ptr.cast().write(slice_mut) };
    }

    #[allow(unstable_name_collisions, clippy::incompatible_msrv)]
    unsafe fn try_reserve<T: Send + Sync + 'static, C: 'static, B: BufferMut<T>>(
        arc: ErasedArc,
        additional: usize,
        allocate: bool,
        start: NonNull<()>,
        length: usize,
    ) -> TryReserveResult<()> {
        unsafe { Self::update_capacity::<T, C, B>(arc) };
        let inner = unsafe { arc.cast::<ArcInner<C, B, ()>>().as_mut() };
        let buffer = &mut inner.buffer;
        let offset = unsafe { start.cast().sub_ptr(buffer.as_mut_ptr()) };
        unsafe {
            match buffer.try_reclaim_or_reserve(offset, length, additional, allocate) {
                Ok(offset) => (
                    Ok(buffer.capacity() - offset),
                    buffer.as_mut_ptr().add(offset).cast(),
                ),
                Err(err) => (Err(err), buffer.as_mut_ptr().add(offset).cast()),
            }
        }
    }

    fn new<T: Send + Sync + 'static, B: Buffer<T>, M: Any>() -> &'static Self {
        macro_rules! vtable {
            (get_metadata: $get_metadata:expr) => {
                &Self {
                    dealloc: Self::dealloc::<(), B, M>,
                    get_metadata: $get_metadata,
                    take_buffer: Self::take_buffer_const::<T, B, M>,
                    into_mut: None,
                    try_reserve: None,
                }
            };
        }
        if is_not!(M, ()) {
            vtable!(get_metadata: Some(Self::get_metadata::<(), B, M>))
        } else {
            vtable!(get_metadata: None)
        }
    }

    fn new_borrow<T: Send + Sync + 'static, B: Buffer<T> + BorrowMetadata>() -> &'static Self {
        &Self {
            dealloc: Self::dealloc::<(), B, ()>,
            get_metadata: Some(Self::get_borrowed_metadata::<(), B>),
            take_buffer: Self::take_buffer_const::<T, B, ()>,
            into_mut: None,
            try_reserve: None,
        }
    }

    fn new_mut<T: Send + Sync + 'static, C: 'static, B: BufferMut<T>, M: 'static>() -> &'static Self
    {
        macro_rules! vtable {
            (get_metadata: $get_metadata:expr) => {
                &Self {
                    dealloc: Self::dealloc_mut::<T, C, B, M>,
                    get_metadata: $get_metadata,
                    take_buffer: Self::take_buffer_mut::<T, C, B, M>,
                    into_mut: Some(Self::into_mut::<T, C, B>),
                    try_reserve: Some(Self::try_reserve::<T, C, B>),
                }
            };
        }
        if is_not!(M, ()) {
            vtable!(get_metadata: Some(Self::get_metadata::<C, B, M>))
        } else {
            vtable!(get_metadata: None)
        }
    }

    fn new_borrow_mut<T: Send + Sync + 'static, C: 'static, B: BufferMut<T> + BorrowMetadata>(
    ) -> &'static Self {
        &Self {
            dealloc: Self::dealloc_mut::<T, C, B, ()>,
            get_metadata: Some(Self::get_borrowed_metadata::<C, B>),
            take_buffer: Self::take_buffer_mut::<T, C, B, ()>,
            into_mut: Some(Self::into_mut::<T, C, B>),
            try_reserve: Some(Self::try_reserve::<T, C, B>),
        }
    }
}

union VTableOrCapacity {
    vtable: &'static VTable,
    capacity: usize,
}

enum VTableOrVec {
    VTable(&'static VTable),
    Vec {
        base: NonNull<()>,
        capacity: NonZero<usize>,
    },
}

const VEC_FLAG: usize = 1;
const VEC_CAPA_SHIFT: usize = 1;

struct ArcGuard<I>(NonNull<I>);
impl<I> ArcGuard<I> {
    fn new(inner: I) -> Self {
        Self(Box::new(inner).into_non_null())
    }

    fn get(&mut self) -> &mut I {
        unsafe { self.0.as_mut() }
    }

    fn into_arc<T: Send + Sync + 'static>(self) -> Arc<T> {
        ManuallyDrop::new(self).0.cast().into()
    }
}

impl<T> Drop for ArcGuard<T> {
    fn drop(&mut self) {
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

pub(crate) struct Arc<T> {
    inner: ErasedArc,
    _phantom: PhantomData<T>,
}

impl<T> From<ErasedArc> for Arc<T> {
    fn from(value: ErasedArc) -> Self {
        Self {
            inner: value,
            _phantom: PhantomData,
        }
    }
}

impl<T> Arc<T> {
    fn vtable_or_vec(&self) -> VTableOrVec {
        let inner = unsafe { self.inner.as_ref() };
        unsafe {
            if inner.vtable_or_capa.capacity & VEC_FLAG != 0 {
                let capacity = NonZero::new(inner.vtable_or_capa.capacity >> VEC_CAPA_SHIFT)
                    .unwrap_unchecked();
                let inner = self.inner.cast::<ArcInner<(), NonNull<()>, ()>>().as_ref();
                let base = inner.buffer;
                VTableOrVec::Vec { base, capacity }
            } else {
                VTableOrVec::VTable(inner.vtable_or_capa.vtable)
            }
        }
    }
}

impl<T: Send + Sync + 'static> Arc<T> {
    fn new_vec(vec: Vec<T>, rc: usize) -> (Self, NonNull<T>, usize, usize) {
        let mut vec = ManuallyDrop::new(vec);
        let capacity = VEC_FLAG | (vec.capacity() << VEC_CAPA_SHIFT);
        let start = NonNull::new(vec.as_mut_ptr()).unwrap();
        let inner = ArcGuard::new(ArcInner {
            rc: rc.into(),
            vtable_or_capa: VTableOrCapacity { capacity },
            spare_capacity: (),
            buffer: start,
            metadata: (),
        });
        (inner.into_arc(), start, vec.len(), vec.capacity())
    }

    fn new_inner<B: Buffer<T>, M>(
        vtable: &'static VTable,
        buffer: B,
        metadata: M,
        rc: usize,
    ) -> (Self, NonNull<T>, usize) {
        let mut inner = ArcGuard::new(ArcInner {
            rc: rc.into(),
            vtable_or_capa: VTableOrCapacity { vtable },
            spare_capacity: (),
            buffer,
            metadata,
        });
        let slice = inner.get().buffer.as_slice();
        let start = NonNull::new(slice.as_ptr().cast_mut()).unwrap();
        let len = slice.len();
        (inner.into_arc(), start, len)
    }

    pub(crate) fn new<B: Buffer<T>, M: Send + Sync + 'static>(
        mut buffer: B,
        metadata: M,
        rc: usize,
    ) -> (Self, NonNull<T>, usize) {
        match buffer.try_into_vec() {
            Ok(vec) => {
                let (arc, start, len, _) = if is!(M, ()) && !mem::needs_drop::<T>() {
                    Self::new_vec(vec, rc)
                } else {
                    Self::new_mut(vec, metadata, rc)
                };
                return (arc, start, len);
            }
            Err(b) => buffer = b,
        }
        Self::new_inner(VTable::new::<T, B, M>(), buffer, metadata, rc)
    }

    pub(crate) fn new_borrow<B: Buffer<T> + BorrowMetadata>(
        buffer: B,
    ) -> (Self, NonNull<T>, usize) {
        Self::new_inner(VTable::new_borrow::<T, B>(), buffer, (), 1)
    }

    fn new_mut_inner<C: 'static, B: BufferMut<T>, M: Send + Sync + 'static>(
        vtable: &'static VTable,
        spare_capacity: C,
        buffer: B,
        metadata: M,
        rc: usize,
    ) -> (Self, NonNull<T>, usize, usize) {
        let mut inner = ArcGuard::new(ArcInner {
            rc: rc.into(),
            vtable_or_capa: VTableOrCapacity { vtable },
            spare_capacity,
            buffer,
            metadata,
        });
        let len = inner.get().buffer.len();
        let capacity = inner.get().buffer.capacity();
        let start = inner.get().buffer.as_mut_ptr();
        (inner.into_arc(), start, len, capacity)
    }

    pub(crate) fn new_mut<B: BufferMut<T>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
        rc: usize,
    ) -> (Self, NonNull<T>, usize, usize) {
        if mem::needs_drop::<T>() {
            let vtable = VTable::new_mut::<T, AtomicUsize, B, M>();
            Self::new_mut_inner(vtable, AtomicUsize::new(usize::MAX), buffer, metadata, rc)
        } else if is!(B, Vec<T>) && is!(M, ()) {
            let Ok(vec) = buffer.try_into_vec() else {
                unreachable!()
            };
            Self::new_vec(vec, rc)
        } else {
            let vtable = VTable::new_mut::<T, (), B, M>();
            Self::new_mut_inner(vtable, (), buffer, metadata, rc)
        }
    }

    pub(crate) fn new_borrow_mut<B: BufferMut<T> + BorrowMetadata>(
        buffer: B,
    ) -> (Self, NonNull<T>, usize, usize) {
        if mem::needs_drop::<T>() {
            let vtable = VTable::new_borrow_mut::<T, AtomicUsize, B>();
            Self::new_mut_inner(vtable, AtomicUsize::new(usize::MAX), buffer, (), 1)
        } else {
            let vtable = VTable::new_borrow_mut::<T, (), B>();
            Self::new_mut_inner(vtable, (), buffer, (), 1)
        }
    }

    pub(crate) unsafe fn forget_vec(self) {
        fn forget<T, C, B>(arc: Arc<T>) {
            let arc = ManuallyDrop::new(arc);
            let inner = arc.inner.cast::<ArcInner<C, B, ()>>();
            mem::forget(*unsafe { Box::from_raw(inner.as_ptr()) });
        }
        match self.vtable_or_vec() {
            VTableOrVec::Vec { .. } => forget::<T, (), NonNull<T>>(self),
            VTableOrVec::VTable(_) => forget::<T, AtomicUsize, Vec<T>>(self),
        }
    }

    pub(crate) unsafe fn from_ptr(ptr: NonNull<()>) -> Self {
        ptr.cast().into()
    }

    pub(crate) fn into_ptr(self) -> NonNull<()> {
        ManuallyDrop::new(self).inner.cast()
    }

    pub(crate) fn get_metadata<'a, M: Any>(&self) -> Option<&'a M> {
        match self.vtable_or_vec() {
            VTableOrVec::VTable(vtable) => match vtable.get_metadata {
                Some(f) => unsafe { Some(f(self.inner, TypeId::of::<M>())?.cast().as_ref()) },
                None if is!(M, ()) => Some(unit_metadata()),
                None => None,
            },
            VTableOrVec::Vec { .. } if is!(M, ()) => Some(unit_metadata()),
            VTableOrVec::Vec { .. } => None,
        }
    }

    pub(crate) fn is_unique(&self) -> bool {
        unsafe { self.inner.as_ref() }.rc.load(Ordering::Relaxed) == 1
    }

    #[allow(clippy::incompatible_msrv)]
    pub(crate) unsafe fn take_buffer<B: Buffer<T>>(
        &mut self,
        len: usize,
        buffer_ptr: NonNull<B>,
    ) -> bool {
        self.is_unique()
            && match self.vtable_or_vec() {
                VTableOrVec::VTable(vtable) => unsafe {
                    (vtable.take_buffer)(self.inner, TypeId::of::<B>(), len, buffer_ptr.cast())
                },
                VTableOrVec::Vec { base, capacity } if is!(B, Vec<T>) => unsafe {
                    let vec = Vec::from_raw_parts(base.cast().as_ptr(), 0, capacity.get());
                    buffer_ptr.cast::<Vec<T>>().write(vec);
                    VTable::dealloc::<(), NonNull<()>, ()>(self.inner);
                    true
                },
                VTableOrVec::Vec { .. } => false,
            }
    }

    pub(crate) unsafe fn try_as_mut(&mut self) -> Option<ArcSliceMut<T>> {
        if !self.is_unique() {
            return None;
        }
        match self.vtable_or_vec() {
            VTableOrVec::VTable(vtable) => {
                let mut slice_mut = MaybeUninit::uninit();
                let slice_mut_ptr = NonNull::new(slice_mut.as_mut_ptr()).unwrap().cast();
                unsafe { vtable.into_mut?(self.inner, slice_mut_ptr) };
                Some(unsafe { slice_mut.assume_init() })
            }
            VTableOrVec::Vec { base, capacity } => {
                let arc = self.inner.into();
                Some(ArcSliceMut::from_arc(base.cast(), 0, capacity.get(), arc))
            }
        }
    }

    pub(crate) unsafe fn set_spare_capacity(&self, spare_capacity: usize) {
        let inner = unsafe { self.inner.cast::<ArcInner<AtomicUsize, (), ()>>().as_ref() };
        inner
            .spare_capacity
            .store(spare_capacity, Ordering::Relaxed);
    }

    #[allow(unstable_name_collisions, clippy::incompatible_msrv)]
    pub(crate) unsafe fn try_reserve(
        &mut self,
        additional: usize,
        allocate: bool,
        start: NonNull<T>,
        length: usize,
    ) -> TryReserveResult<T> {
        if !self.is_unique() {
            return (Err(TryReserveError::NotUnique), start);
        }
        match self.vtable_or_vec() {
            VTableOrVec::VTable(vtable) => unsafe {
                let (res, start) = vtable.try_reserve.unwrap_unchecked()(
                    self.inner,
                    additional,
                    allocate,
                    start.cast(),
                    length,
                );
                (res, start.cast())
            },
            VTableOrVec::Vec { base, capacity } => {
                let base = base.cast::<T>();
                let offset = unsafe { start.sub_ptr(base) };
                let mut vec = ManuallyDrop::new(unsafe {
                    Vec::from_raw_parts(base.as_ptr(), 0, capacity.get())
                });
                match unsafe { vec.try_reclaim_or_reserve(offset, length, additional, allocate) } {
                    Ok(offset) => {
                        let base = BufferMut::as_mut_ptr(&mut *vec);
                        let inner =
                            unsafe { self.inner.cast::<ArcInner<(), NonNull<T>, ()>>().as_mut() };
                        inner.vtable_or_capa = VTableOrCapacity {
                            capacity: VEC_FLAG | (vec.capacity() << VEC_CAPA_SHIFT),
                        };
                        inner.buffer = base;
                        (Ok(vec.capacity() - offset), unsafe { base.add(offset) })
                    }
                    Err(err) => (Err(err), unsafe {
                        BufferMut::as_mut_ptr(&mut *vec).add(offset)
                    }),
                }
            }
        }
    }
}

impl<T> Drop for Arc<T> {
    fn drop(&mut self) {
        let inner = unsafe { self.inner.as_ref() };
        // See `Arc` documentation
        if inner.rc.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        atomic::fence(Ordering::Acquire);
        #[cold]
        fn dealloc<T>(inner: ErasedArc) {
            match ManuallyDrop::new(Arc::<T>::from(inner)).vtable_or_vec() {
                VTableOrVec::VTable(vtable) => unsafe { (vtable.dealloc)(inner) },
                VTableOrVec::Vec { base, capacity } => {
                    let base = base.cast::<T>();
                    drop(unsafe { Vec::from_raw_parts(base.as_ptr(), 0, capacity.get()) });
                    unsafe { VTable::dealloc::<(), NonNull<()>, ()>(inner) };
                }
            }
        }
        dealloc::<T>(self.inner);
    }
}

impl<T> Clone for Arc<T> {
    fn clone(&self) -> Self {
        let inner = unsafe { self.inner.as_ref() };
        // See `Arc` documentation
        let old_size = inner.rc.fetch_add(1, Ordering::Relaxed);
        const MAX_REFCOUNT: usize = isize::MAX as usize;
        if old_size > MAX_REFCOUNT {
            abort();
        }
        Self {
            inner: self.inner,
            _phantom: PhantomData,
        }
    }
}

#[inline(never)]
#[cold]
pub(crate) fn abort() -> ! {
    #[cfg(feature = "std")]
    {
        extern crate std;
        std::process::abort();
    }
    // in no_std, use double panic
    #[cfg(not(feature = "std"))]
    {
        struct Abort;
        impl Drop for Abort {
            fn drop(&mut self) {
                panic!();
            }
        }
        let _a = Abort;
        panic!("abort");
    }
}
