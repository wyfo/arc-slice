use alloc::{boxed::Box, vec::Vec};
use core::{
    any::Any,
    hint, mem,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::NonNull,
};

#[allow(unused_imports)]
use crate::msrv::{BoolExt, StrictProvenance};
use crate::{
    arc::Arc,
    atomic::{AtomicPtr, Ordering},
    buffer::{Buffer, BufferExt, BufferMut, BufferMutExt, Slice, SliceExt},
    layout::{BoxedSliceLayout, VecLayout},
    macros::is,
    msrv::{ptr, NonZero, SubPtrExt},
    slice::ArcSliceLayout,
    slice_mut,
    slice_mut::ArcSliceMutLayout,
    utils::{transmute_checked, try_transmute},
};

const CAPACITY_FLAG: usize = 1;
const CAPACITY_SHIFT: usize = 1;

enum Data<S: Slice + ?Sized> {
    Static,
    Arc(ManuallyDrop<Arc<S>>),
    Capacity(NonZero<usize>),
}

impl<S: Slice + ?Sized> Data<S> {
    #[inline(always)]
    fn from_ptr(ptr: *mut ()) -> Self {
        match NonNull::new(ptr) {
            Some(_) if ptr.addr() & CAPACITY_FLAG != 0 => {
                Data::Capacity(unsafe { NonZero::new_unchecked(ptr.addr() >> CAPACITY_SHIFT) })
            }
            Some(arc) => Data::Arc(ManuallyDrop::new(unsafe { Arc::from_raw(arc) })),
            None => Data::Static,
        }
    }
}

#[allow(missing_debug_implementations)]
pub struct DataPtr(AtomicPtr<()>);

impl DataPtr {
    const fn new_static() -> Self {
        Self(AtomicPtr::new(ptr::null_mut()))
    }

    fn capacity_as_ptr(capacity: usize) -> *mut () {
        ptr::without_provenance_mut::<()>(CAPACITY_FLAG | (capacity << CAPACITY_SHIFT))
    }

    fn new_capacity(capacity: usize) -> Self {
        Self(AtomicPtr::new(Self::capacity_as_ptr(capacity)))
    }

    fn new_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(arc: Arc<S, ANY_BUFFER>) -> Self {
        Self(AtomicPtr::new(arc.into_raw().as_ptr()))
    }

    fn get<S: Slice + ?Sized>(&self) -> Data<S> {
        Data::from_ptr(self.0.load(Ordering::Acquire))
    }

    fn get_mut<S: Slice + ?Sized>(&mut self) -> Data<S> {
        Data::from_ptr(*self.0.get_mut())
    }

    #[cold]
    fn promote_vec<S: Slice + ?Sized>(&self, vec: S::Vec) -> DataPtr {
        let capacity = vec.capacity();
        let guard = Arc::<S>::promote_vec(vec);
        // Release ordering must be used to ensure the arc vtable is visible
        // by `get_metadata`. In case of failure, the read arc is cloned with
        // a fetch-and-add, so there is no need of synchronization.
        let arc = match self.0.compare_exchange(
            Self::capacity_as_ptr(capacity),
            guard.as_ptr(),
            if cfg!(feature = "const-slice") {
                Ordering::AcqRel
            } else {
                Ordering::Release
            },
            Ordering::Acquire,
        ) {
            Ok(_) => guard.into(),
            Err(ptr) => match Data::from_ptr(ptr) {
                Data::Arc(arc) => (*arc).clone(),
                _ => unsafe { hint::unreachable_unchecked() },
            },
        };
        Self::new_arc(arc)
    }
}

pub trait BoxedSliceOrVecLayout {
    type Base: Copy;
    const TRUNCATABLE: bool;
    fn get_base<S: Slice + ?Sized>(_vec: &mut S::Vec) -> Option<Self::Base>;
    unsafe fn rebuild_vec<S: Slice + ?Sized>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: NonZero<usize>,
        base: MaybeUninit<Self::Base>,
    ) -> S::Vec;
}

impl BoxedSliceOrVecLayout for BoxedSliceLayout {
    type Base = ();

    const TRUNCATABLE: bool = false;

    fn get_base<S: Slice + ?Sized>(vec: &mut S::Vec) -> Option<Self::Base> {
        (vec.len() == vec.capacity()).then_some(())
    }

    unsafe fn rebuild_vec<S: Slice + ?Sized>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: NonZero<usize>,
        _base: MaybeUninit<Self::Base>,
    ) -> S::Vec {
        let offset = capacity.get() - length;
        let ptr = unsafe { start.as_ptr().sub(offset) };
        unsafe { S::from_vec_unchecked(Vec::from_raw_parts(ptr, capacity.get(), capacity.get())) }
    }
}

impl BoxedSliceOrVecLayout for VecLayout {
    type Base = NonNull<()>;

    const TRUNCATABLE: bool = true;

    fn get_base<S: Slice + ?Sized>(vec: &mut S::Vec) -> Option<Self::Base> {
        Some(S::vec_start(vec).cast())
    }

    #[allow(unstable_name_collisions)]
    unsafe fn rebuild_vec<S: Slice + ?Sized>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: NonZero<usize>,
        base: MaybeUninit<Self::Base>,
    ) -> S::Vec {
        let base = unsafe { base.assume_init().cast() };
        let len = unsafe { start.sub_ptr(base) } + length;
        unsafe { S::from_vec_unchecked(Vec::from_raw_parts(base.as_ptr(), len, capacity.get())) }
    }
}

unsafe impl<L: BoxedSliceOrVecLayout + 'static> ArcSliceLayout for L {
    type Data = (DataPtr, MaybeUninit<L::Base>);
    #[allow(clippy::declare_interior_mutable_const)]
    const STATIC_DATA: Option<Self::Data> = Some((DataPtr::new_static(), MaybeUninit::uninit()));
    #[allow(clippy::declare_interior_mutable_const)]
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> =
        MaybeUninit::new((DataPtr::new_static(), MaybeUninit::uninit()));

    fn data_from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(
        arc: Arc<S, ANY_BUFFER>,
    ) -> Self::Data {
        (DataPtr::new_arc(arc), MaybeUninit::uninit())
    }

    fn data_from_vec<S: Slice + ?Sized>(mut vec: S::Vec) -> Self::Data {
        if let Some(base) = L::get_base::<S>(&mut vec) {
            let capacity = ManuallyDrop::new(vec).capacity();
            (DataPtr::new_capacity(capacity), MaybeUninit::new(base))
        } else {
            let arc = Arc::<S>::new_vec(vec);
            (DataPtr::new_arc(arc), MaybeUninit::uninit())
        }
    }

    fn clone<S: Slice + ?Sized>(
        start: NonNull<S::Item>,
        length: usize,
        data: &Self::Data,
    ) -> Self::Data {
        let (ptr, base) = data;
        let new_ptr = match ptr.get::<S>() {
            Data::Static => DataPtr::new_static(),
            Data::Arc(arc) => DataPtr::new_arc((*arc).clone()),
            Data::Capacity(capacity) => {
                let vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, *base) };
                data.0.promote_vec::<S>(vec)
            }
        };
        (new_ptr, MaybeUninit::uninit())
    }

    unsafe fn drop<S: Slice + ?Sized, const UNIQUE_HINT: bool>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) {
        let (ptr, base) = &mut **data;
        match ptr.get_mut::<S>() {
            Data::Static => {}
            Data::Arc(arc) => ManuallyDrop::into_inner(arc).drop_with_unique_hint::<UNIQUE_HINT>(),
            Data::Capacity(capacity) => {
                drop(unsafe { Self::rebuild_vec::<S>(start, length, capacity, *base) });
            }
        }
    }

    fn truncate<S: Slice + ?Sized>(start: NonNull<S::Item>, length: usize, data: &mut Self::Data) {
        let (ptr, base) = data;
        if !Self::TRUNCATABLE || S::needs_drop() {
            if let Data::Capacity(capacity) = ptr.get_mut::<S>() {
                let vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, *base) };
                *ptr = DataPtr::new_arc(Arc::<S>::new_vec(vec));
            }
        }
    }

    fn is_unique<S: Slice + ?Sized>(data: &Self::Data) -> bool {
        let (ptr, _) = data;
        match ptr.get::<S>() {
            Data::Static => false,
            Data::Arc(arc) => arc.is_buffer_unique(),
            Data::Capacity(_) => true,
        }
    }

    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Self::Data) -> Option<&M> {
        let (ptr, _) = data;
        match ptr.get::<S>() {
            Data::Arc(arc) => Some(unsafe { &*ptr::from_ref(arc.get_metadata::<M>()?) }),
            _ => None,
        }
    }

    #[allow(unstable_name_collisions)]
    unsafe fn take_buffer<S: Slice + ?Sized, B: Buffer<S>>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B> {
        let (ptr, base) = &mut **data;
        match ptr.get_mut::<S>() {
            Data::Static => {
                try_transmute(unsafe { S::from_raw_parts::<'static>(start, length) }).ok()
            }
            Data::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_buffer::<B, false>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            Data::Capacity(capacity) if is!(B, S::Vec) => {
                let mut vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, *base) };
                let offset = unsafe { vec.offset(start) };
                unsafe { vec.shift_left(offset, length, S::vec_start) };
                Some(transmute_checked(vec))
            }
            Data::Capacity(capacity) if is!(B, Box<S>) && length == capacity.get() => {
                let slice = ptr::slice_from_raw_parts_mut(start.as_ptr(), length);
                let boxed_slice = unsafe { S::from_boxed_slice_unchecked(Box::from_raw(slice)) };
                Some(transmute_checked(boxed_slice))
            }
            Data::Capacity(_) => None,
        }
    }

    unsafe fn take_array<T: Send + Sync + 'static, const N: usize>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<[T; N]> {
        let (ptr, _) = &mut **data;
        match ptr.get_mut::<[T]>() {
            Data::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_array::<N, false>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            _ => None,
        }
    }

    #[allow(unstable_name_collisions)]
    unsafe fn mut_data<S: Slice + ?Sized, L2: ArcSliceMutLayout>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<slice_mut::Data>)> {
        let (ptr, base) = &mut **data;
        match ptr.get_mut::<S>() {
            Data::Static => (length == 0).then_some((0, None)),
            Data::Arc(mut arc) => Some((
                unsafe { arc.capacity(start)? },
                Some(ManuallyDrop::into_inner(arc).into()),
            )),
            Data::Capacity(capacity) => {
                let vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, *base) };
                let offset = unsafe { vec.offset(start) };
                let data = Some(unsafe { L2::data_from_vec::<S>(vec, offset) });
                Some((capacity.get() - offset, data))
            }
        }
    }

    unsafe fn update_layout<S: Slice + ?Sized, L2: ArcSliceLayout>(
        start: NonNull<S::Item>,
        length: usize,
        data: Self::Data,
    ) -> L2::Data {
        let (mut ptr, base) = data;
        match ptr.get_mut::<S>() {
            Data::Static => L2::data_from_static(unsafe { S::from_raw_parts(start, length) }),
            Data::Arc(arc) => L2::data_from_arc(ManuallyDrop::into_inner(arc)),
            Data::Capacity(capacity) => L2::data_from_vec::<S>(unsafe {
                Self::rebuild_vec::<S>(start, length, capacity, base)
            }),
        }
    }
}
