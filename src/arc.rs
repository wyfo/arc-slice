use alloc::{boxed::Box, vec::Vec};
use core::{
    self,
    any::{Any, TypeId},
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ptr,
    ptr::{addr_of, addr_of_mut, NonNull},
};

use crate::{
    buffer::{can_reclaim, Buffer, BufferMut, TryReserveError},
    loom::{
        atomic_usize_with_mut,
        sync::{
            atomic,
            atomic::{AtomicUsize, Ordering},
        },
    },
    macros::{is, is_not},
    rust_compat::{box_into_nonnull, non_null_add, non_null_write, ptr_from_mut, sub_ptr},
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
    vtable: &'static VTable,
    spare_capacity: C,
    buffer: B,
    metadata: M,
}

type ErasedArc = NonNull<ArcInner<(), (), ()>>;
type TryReserveResult<T> = (Result<usize, TryReserveError>, NonNull<T>);

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
    unsafe fn update_capacity<T: Send + Sync + 'static, B: BufferMut<T>>(arc: ErasedArc) {
        let inner = unsafe { arc.cast::<ArcInner<AtomicUsize, B, ()>>().as_mut() };
        let spare_capacity = atomic_usize_with_mut(&mut inner.spare_capacity, |c| *c);
        if spare_capacity != usize::MAX {
            let len = inner.buffer.capacity() - spare_capacity;
            unsafe { inner.buffer.set_len(len) };
        }
    }

    unsafe fn dealloc<C, B, M>(arc: ErasedArc) {
        drop(unsafe { Box::from_raw(arc.cast::<ArcInner<C, B, M>>().as_ptr()) });
    }

    unsafe fn dealloc_mut<T: Send + Sync + 'static, B: BufferMut<T>, M>(arc: ErasedArc) {
        unsafe { Self::update_capacity::<T, B>(arc) }
        unsafe { Self::dealloc::<AtomicUsize, B, M>(arc) }
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
        let inner_ptr = ptr_from_mut(inner);
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

    unsafe fn take_buffer_mut<T: Send + Sync + 'static, B: BufferMut<T>, M>(
        arc: ErasedArc,
        type_id: TypeId,
        len: usize,
        buffer_ptr: NonNull<()>,
    ) -> bool {
        let buffer_len = |buffer: &B| buffer.len();
        unsafe { Self::update_capacity::<T, B>(arc) };
        unsafe {
            Self::take_buffer::<T, AtomicUsize, B, M>(arc, type_id, len, buffer_ptr, buffer_len)
        }
    }

    unsafe fn into_mut<T: Send + Sync + 'static, B: BufferMut<T>>(
        arc: ErasedArc,
        slice_mut_ptr: NonNull<()>,
    ) {
        unsafe { Self::update_capacity::<T, B>(arc) };
        let inner = unsafe { arc.cast::<ArcInner<AtomicUsize, B, ()>>().as_mut() };
        let slice_mut = ArcSliceMut::from_arc(&mut inner.buffer, Arc(arc));
        unsafe { non_null_write(slice_mut_ptr.cast(), slice_mut) };
    }

    unsafe fn try_reserve<T: Send + Sync + 'static, B: BufferMut<T>>(
        arc: ErasedArc,
        additional: usize,
        allocate: bool,
        start: NonNull<()>,
        length: usize,
    ) -> TryReserveResult<()> {
        unsafe { Self::update_capacity::<T, B>(arc) };
        let inner = unsafe { arc.cast::<ArcInner<AtomicUsize, B, ()>>().as_mut() };
        let base = inner.buffer.as_mut_ptr();
        let offset = unsafe { sub_ptr(start.as_ptr().cast::<T>(), base.as_ptr()) };
        if can_reclaim(inner.buffer.capacity(), offset, length, additional)
            && inner.buffer.shift_left(offset)
        {
            return (
                Ok(inner.buffer.capacity()),
                inner.buffer.as_mut_ptr().cast(),
            );
        } else if !allocate || !inner.buffer.truncate(offset + length) {
            let start = unsafe { non_null_add(base, offset).cast() };
            return (Err(TryReserveError::Unsupported), start);
        }
        let reserve_res = inner.buffer.try_reserve(additional);
        let capacity = inner.buffer.capacity();
        let start = unsafe { non_null_add(inner.buffer.as_mut_ptr(), offset).cast() };
        match reserve_res {
            Ok(_) => (Ok(capacity - offset), start),
            Err(err) => (Err(err), start),
        }
    }

    fn new<T: Send + Sync + 'static, B: Buffer<T>, M: Any>() -> &'static Self {
        if is_not!(M, ()) {
            &Self {
                dealloc: Self::dealloc::<(), B, M>,
                get_metadata: Some(Self::get_metadata::<(), B, M>),
                take_buffer: Self::take_buffer_const::<T, B, M>,
                into_mut: None,
                try_reserve: None,
            }
        } else {
            &Self {
                dealloc: Self::dealloc::<(), B, M>,
                get_metadata: None,
                take_buffer: Self::take_buffer_const::<T, B, M>,
                into_mut: None,
                try_reserve: None,
            }
        }
    }

    fn new_mut<T: Send + Sync + 'static, B: BufferMut<T>, M: 'static>() -> &'static Self {
        if is_not!(M, ()) {
            &Self {
                dealloc: Self::dealloc_mut::<T, B, M>,
                get_metadata: Some(Self::get_metadata::<AtomicUsize, B, M>),
                take_buffer: Self::take_buffer_mut::<T, B, M>,
                into_mut: Some(Self::into_mut::<T, B>),
                try_reserve: Some(Self::try_reserve::<T, B>),
            }
        } else {
            &Self {
                dealloc: Self::dealloc_mut::<T, B, M>,
                get_metadata: None,
                take_buffer: Self::take_buffer_mut::<T, B, M>,
                into_mut: Some(Self::into_mut::<T, B>),
                try_reserve: Some(Self::try_reserve::<T, B>),
            }
        }
    }
}

struct ArcGuard<T>(NonNull<T>);
impl<T> ArcGuard<T> {
    fn new(inner: T) -> Self {
        Self(box_into_nonnull(Box::new(inner)))
    }

    fn get(&mut self) -> &mut T {
        unsafe { self.0.as_mut() }
    }

    fn into_arc(self) -> Arc {
        Arc(ManuallyDrop::new(self).0.cast())
    }
}

impl<T> Drop for ArcGuard<T> {
    fn drop(&mut self) {
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

pub(crate) struct Arc(ErasedArc);

impl Arc {
    pub(crate) fn new<T: Send + Sync + 'static, B: Buffer<T>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
        rc: usize,
    ) -> (Self, NonNull<T>, usize) {
        let mut inner = ArcGuard::new(ArcInner {
            rc: rc.into(),
            vtable: VTable::new::<T, B, M>(),
            spare_capacity: (),
            buffer,
            metadata,
        });
        let slice = inner.get().buffer.as_slice();
        let start = NonNull::new(slice.as_ptr().cast_mut()).unwrap();
        let len = slice.len();
        (inner.into_arc(), start, len)
    }

    pub(crate) fn new_mut<T: Send + Sync + 'static, B: BufferMut<T>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
        rc: usize,
    ) -> (Self, NonNull<T>, usize, usize) {
        let mut inner = ArcGuard::new(ArcInner {
            rc: rc.into(),
            vtable: VTable::new_mut::<T, B, M>(),
            spare_capacity: AtomicUsize::new(usize::MAX),
            buffer,
            metadata,
        });
        let len = inner.get().buffer.len();
        let capacity = inner.get().buffer.capacity();
        let start = inner.get().buffer.as_mut_ptr();
        (inner.into_arc(), start, len, capacity)
    }

    pub(crate) unsafe fn forget_vec<T>(self) {
        let this = ManuallyDrop::new(self);
        let inner = this.0.cast::<ArcInner<AtomicUsize, Vec<T>, ()>>();
        mem::forget(*unsafe { Box::from_raw(inner.as_ptr()) });
    }

    pub(crate) unsafe fn from_ptr(ptr: NonNull<()>) -> Self {
        Self(ptr.cast())
    }

    pub(crate) fn into_ptr(self) -> NonNull<()> {
        ManuallyDrop::new(self).0.cast()
    }

    pub(crate) fn get_metadata<'a, M: Any>(&self) -> Option<&'a M> {
        let inner = unsafe { self.0.as_ref() };
        match inner.vtable.get_metadata {
            Some(f) => unsafe { Some(f(self.0, TypeId::of::<M>())?.cast().as_ref()) },
            None if is!(M, ()) => Some(unit_metadata()),
            None => None,
        }
    }

    pub(crate) fn is_unique(&self) -> bool {
        unsafe { self.0.as_ref() }.rc.load(Ordering::Relaxed) == 1
    }

    pub(crate) unsafe fn take_buffer<B: Any>(
        &mut self,
        len: usize,
        buffer_ptr: NonNull<B>,
    ) -> bool {
        let inner = unsafe { self.0.as_ref() };
        self.is_unique()
            && unsafe {
                (inner.vtable.take_buffer)(self.0, TypeId::of::<B>(), len, buffer_ptr.cast())
            }
    }

    pub(crate) unsafe fn try_as_mut<T: Send + Sync + 'static>(&mut self) -> Option<ArcSliceMut<T>> {
        let inner = unsafe { self.0.as_ref() };
        if !self.is_unique() {
            return None;
        }
        let mut slice_mut = MaybeUninit::uninit();
        let slice_mut_ptr = NonNull::new(slice_mut.as_mut_ptr()).unwrap().cast();
        unsafe { inner.vtable.into_mut?(self.0, slice_mut_ptr) }
        Some(unsafe { slice_mut.assume_init() })
    }

    pub(crate) fn set_spare_capacity(&self, spare_capacity: usize) {
        let inner = unsafe { self.0.cast::<ArcInner<AtomicUsize, (), ()>>().as_ref() };
        inner
            .spare_capacity
            .store(spare_capacity, Ordering::Relaxed);
    }

    pub(crate) unsafe fn try_reserve<T>(
        &mut self,
        additional: usize,
        allocate: bool,
        start: NonNull<T>,
        length: usize,
    ) -> (Result<usize, TryReserveError>, NonNull<T>) {
        let inner = unsafe { self.0.as_ref() };
        if !self.is_unique() {
            return (Err(TryReserveError::NotUnique), start);
        }
        let (res, start) = unsafe {
            inner.vtable.try_reserve.unwrap_unchecked()(
                self.0,
                additional,
                allocate,
                start.cast(),
                length,
            )
        };
        (res, start.cast())
    }
}

impl Drop for Arc {
    fn drop(&mut self) {
        let inner = unsafe { self.0.as_ref() };
        // See `Arc` documentation
        if inner.rc.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        atomic::fence(Ordering::Acquire);
        unsafe { (inner.vtable.dealloc)(self.0) }
    }
}

impl Clone for Arc {
    fn clone(&self) -> Self {
        let inner = unsafe { self.0.as_ref() };
        // See `Arc` documentation
        let old_size = inner.rc.fetch_add(1, Ordering::Relaxed);
        const MAX_REFCOUNT: usize = isize::MAX as usize;
        if old_size > MAX_REFCOUNT {
            abort();
        }
        Self(self.0)
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
