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
    buffer::{Buffer, BufferMutExt},
    layout::{BoxedSliceLayout, VecLayout},
    macros::is,
    msrv::{ptr, NonZero, SubPtrExt},
    slice::ArcSliceLayout,
    slice_mut,
    slice_mut::ArcSliceMutLayout,
    utils::{static_slice, transmute_checked, try_transmute, NewChecked},
};

const CAPACITY_FLAG: usize = 1;
const CAPACITY_SHIFT: usize = 1;

enum Data<T> {
    Static,
    Arc(ManuallyDrop<Arc<T>>),
    Capacity(NonZero<usize>),
}

impl<T> Data<T> {
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

    fn new_arc<T, const ANY_BUFFER: bool>(arc: Arc<T, ANY_BUFFER>) -> Self {
        Self(AtomicPtr::new(arc.into_raw().as_ptr()))
    }

    fn get<T>(&self) -> Data<T> {
        Data::from_ptr(self.0.load(Ordering::Acquire))
    }

    fn get_mut<T>(&mut self) -> Data<T> {
        Data::from_ptr(*self.0.get_mut())
    }

    #[cold]
    fn promote_vec<T: Send + Sync + 'static>(&self, vec: Vec<T>) -> DataPtr {
        let capacity = vec.capacity();
        let guard = Arc::promote_vec(vec);
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
    fn get_base<T>(_vec: &mut Vec<T>) -> Option<Self::Base>;
    unsafe fn rebuild_vec<T>(
        start: NonNull<T>,
        length: usize,
        capacity: NonZero<usize>,
        base: MaybeUninit<Self::Base>,
    ) -> Vec<T>;
}

impl BoxedSliceOrVecLayout for BoxedSliceLayout {
    type Base = ();

    const TRUNCATABLE: bool = false;

    fn get_base<T>(vec: &mut Vec<T>) -> Option<Self::Base> {
        (vec.len() == vec.capacity()).then_some(())
    }

    unsafe fn rebuild_vec<T>(
        start: NonNull<T>,
        length: usize,
        capacity: NonZero<usize>,
        _base: MaybeUninit<Self::Base>,
    ) -> Vec<T> {
        let offset = capacity.get() - length;
        let ptr = unsafe { start.as_ptr().sub(offset) };
        unsafe { Vec::from_raw_parts(ptr, capacity.get(), capacity.get()) }
    }
}

impl BoxedSliceOrVecLayout for VecLayout {
    type Base = NonNull<()>;

    const TRUNCATABLE: bool = true;

    fn get_base<T>(vec: &mut Vec<T>) -> Option<Self::Base> {
        Some(NonNull::new_checked(vec.as_mut_ptr()).cast())
    }

    #[allow(unstable_name_collisions)]
    unsafe fn rebuild_vec<T>(
        start: NonNull<T>,
        length: usize,
        capacity: NonZero<usize>,
        base: MaybeUninit<Self::Base>,
    ) -> Vec<T> {
        let base = unsafe { base.assume_init().cast() };
        let len = unsafe { start.sub_ptr(base) } + length;
        unsafe { Vec::from_raw_parts(base.as_ptr(), len, capacity.get()) }
    }
}

impl<L: BoxedSliceOrVecLayout + 'static> ArcSliceLayout for L {
    type Data = (DataPtr, MaybeUninit<L::Base>);
    #[allow(clippy::declare_interior_mutable_const)]
    const STATIC_DATA: Option<Self::Data> = Some((DataPtr::new_static(), MaybeUninit::uninit()));
    #[allow(clippy::declare_interior_mutable_const)]
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> =
        MaybeUninit::new((DataPtr::new_static(), MaybeUninit::uninit()));

    fn data_from_arc<T, const ANY_BUFFER: bool>(arc: Arc<T, ANY_BUFFER>) -> Self::Data {
        (DataPtr::new_arc(arc), MaybeUninit::uninit())
    }

    fn data_from_vec<T: Send + Sync + 'static>(mut vec: Vec<T>) -> Self::Data {
        if let Some(base) = L::get_base(&mut vec) {
            let capacity = ManuallyDrop::new(vec).capacity();
            (DataPtr::new_capacity(capacity), MaybeUninit::new(base))
        } else {
            (DataPtr::new_arc(Arc::new_vec(vec)), MaybeUninit::uninit())
        }
    }

    fn clone<T: Send + Sync + 'static>(
        start: NonNull<T>,
        length: usize,
        data: &Self::Data,
    ) -> Self::Data {
        let (ptr, base) = data;
        let new_ptr = match ptr.get::<T>() {
            Data::Static => DataPtr::new_static(),
            Data::Arc(arc) => DataPtr::new_arc((*arc).clone()),
            Data::Capacity(capacity) => {
                let vec = unsafe { Self::rebuild_vec(start, length, capacity, *base) };
                data.0.promote_vec(vec)
            }
        };
        (new_ptr, MaybeUninit::uninit())
    }

    unsafe fn drop<T, const UNIQUE_HINT: bool>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) {
        let (ptr, base) = &mut **data;
        match ptr.get_mut::<T>() {
            Data::Static => {}
            Data::Arc(arc) => ManuallyDrop::into_inner(arc).drop::<UNIQUE_HINT>(),
            Data::Capacity(capacity) => {
                drop(unsafe { Self::rebuild_vec(start, length, capacity, *base) });
            }
        }
    }

    fn truncate<T: Send + Sync + 'static>(start: NonNull<T>, length: usize, data: &mut Self::Data) {
        let (ptr, base) = data;
        if !Self::TRUNCATABLE || mem::needs_drop::<T>() {
            if let Data::Capacity(capacity) = ptr.get_mut::<T>() {
                let vec = unsafe { Self::rebuild_vec(start, length, capacity, *base) };
                *ptr = DataPtr::new_arc(Arc::new_vec(vec));
            }
        }
    }

    fn is_unique<T>(data: &Self::Data) -> bool {
        let (ptr, _) = data;
        match ptr.get::<T>() {
            Data::Static => false,
            Data::Arc(arc) => arc.is_buffer_unique(),
            Data::Capacity(_) => true,
        }
    }

    fn get_metadata<T, M: Any>(data: &Self::Data) -> Option<&M> {
        let (ptr, _) = data;
        match ptr.get::<T>() {
            Data::Arc(arc) => Some(unsafe { &*ptr::from_ref(arc.get_metadata::<M>()?) }),
            _ => None,
        }
    }

    #[allow(unstable_name_collisions)]
    unsafe fn take_buffer<T: Send + Sync + 'static, B: Buffer<T>>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B> {
        let (ptr, base) = &mut **data;
        match ptr.get_mut::<T>() {
            Data::Static => try_transmute(unsafe { static_slice(start, length) }).ok(),
            Data::Arc(arc) => ManuallyDrop::into_inner(arc)
                .take_buffer(start, length)
                .map_err(mem::forget)
                .ok(),
            Data::Capacity(capacity) if is!(B, Vec<T>) => {
                let mut vec = unsafe { Self::rebuild_vec(start, length, capacity, *base) };
                let offset = unsafe { start.as_ptr().sub_ptr(vec.as_mut_ptr()) };
                unsafe { vec.shift_left(offset, length) };
                Some(transmute_checked(vec))
            }
            Data::Capacity(capacity) if is!(B, Box<[T]>) && length == capacity.get() => {
                let slice = ptr::slice_from_raw_parts_mut(start.as_ptr(), length);
                Some(transmute_checked(unsafe { Box::from_raw(slice) }))
            }
            Data::Capacity(_) => None,
        }
    }

    #[allow(unstable_name_collisions)]
    unsafe fn mut_data<T: Send + Sync + 'static, L2: ArcSliceMutLayout>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<slice_mut::Data<true>>)> {
        let (ptr, base) = &mut **data;
        match ptr.get_mut::<T>() {
            Data::Static => (length == 0).then_some((0, None)),
            Data::Arc(mut arc) => {
                arc.is_unique().then_some(())?;
                let capacity = unsafe { arc.capacity(start)? };
                let data = Some(ManuallyDrop::into_inner(arc).into());
                Some((capacity, data))
            }
            Data::Capacity(capacity) => {
                let mut vec = unsafe { Self::rebuild_vec(start, length, capacity, *base) };
                let offset = unsafe { start.as_ptr().sub_ptr(vec.as_mut_ptr()) };
                let data = Some(unsafe { L2::data_from_vec(vec, offset) });
                Some((capacity.get() - offset, data))
            }
        }
    }

    unsafe fn update_layout<T: Send + Sync + 'static, L2: ArcSliceLayout>(
        start: NonNull<T>,
        length: usize,
        data: Self::Data,
    ) -> L2::Data {
        let (mut ptr, base) = data;
        match ptr.get_mut::<T>() {
            Data::Static => L2::data_from_static(unsafe { static_slice(start, length) }),
            Data::Arc(arc) => L2::data_from_arc(ManuallyDrop::into_inner(arc)),
            Data::Capacity(capacity) => {
                L2::data_from_vec(unsafe { Self::rebuild_vec(start, length, capacity, base) })
            }
        }
    }
}
