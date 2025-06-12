use core::{any::Any, mem, mem::ManuallyDrop, ptr::NonNull};

#[allow(unused_imports)]
use crate::msrv::StrictProvenance;
use crate::{
    arc::Arc,
    buffer::{BufferMut, Slice},
    error::AllocErrorImpl,
    layout::ArcLayout,
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut::{ArcSliceMutLayout, Data, TryReserveResult},
    utils::assert_checked,
};
#[cfg(feature = "default-layout-mut-shared")]
use crate::{msrv::NonZero, utils::UnwrapChecked};

#[cfg(feature = "default-layout-mut-shared")]
const SHARED_FLAG: usize = 0b01;

impl<const UNIQUE: bool> Data<UNIQUE> {
    #[inline(always)]
    fn from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(arc: Arc<S, ANY_BUFFER>) -> Self {
        let ptr = arc.into_raw();
        #[cfg(feature = "default-layout-mut-shared")]
        let ptr = if !UNIQUE {
            ptr.map_addr(|addr| NonZero::new(addr.get() | SHARED_FLAG).unwrap_checked())
        } else {
            ptr
        };
        Data(ptr)
    }

    #[inline(always)]
    fn get_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(
        &self,
    ) -> ManuallyDrop<Arc<S, ANY_BUFFER>> {
        let ptr = self.0;
        #[cfg(feature = "default-layout-mut-shared")]
        // MSRV 1.79 NonZero
        let ptr = if !UNIQUE {
            ptr.map_addr(|addr| unsafe { NonZero::new_unchecked(addr.get() & !SHARED_FLAG) })
        } else {
            ptr
        };
        ManuallyDrop::new(unsafe { Arc::from_raw(ptr) })
    }

    #[cfg(not(feature = "default-layout-mut-shared"))]
    #[inline(always)]
    fn is_unique(&self) -> bool {
        UNIQUE
    }

    #[cfg(feature = "default-layout-mut-shared")]
    #[inline(always)]
    fn is_unique(&self) -> bool {
        UNIQUE || self.0.addr().get() & SHARED_FLAG == 0
    }

    #[cfg(not(feature = "default-layout-mut-shared"))]
    #[inline(always)]
    fn make_unique(&mut self) {}

    #[cfg(feature = "default-layout-mut-shared")]
    #[inline(always)]
    fn make_unique(&mut self) {
        self.0 = self
            .0
            .map_addr(|addr| unsafe { NonZero::new_unchecked(addr.get() & !SHARED_FLAG) });
    }

    #[cfg(not(feature = "default-layout-mut-shared"))]
    #[inline(always)]
    fn make_shared(&mut self) {}

    #[cfg(feature = "default-layout-mut-shared")]
    #[inline(always)]
    fn make_shared(&mut self) {
        self.0 = self
            .0
            .map_addr(|addr| unsafe { NonZero::new_unchecked(addr.get() | SHARED_FLAG) });
    }
}

unsafe impl<const ANY_BUFFER: bool, const STATIC: bool> ArcSliceMutLayout
    for ArcLayout<ANY_BUFFER, STATIC>
{
    const ANY_BUFFER: bool = ANY_BUFFER;
    fn try_data_from_arc<S: Slice + ?Sized, const ANY_BUFFER2: bool, const UNIQUE: bool>(
        arc: ManuallyDrop<Arc<S, ANY_BUFFER2>>,
    ) -> Option<Data<UNIQUE>> {
        ManuallyDrop::into_inner(arc)
            .try_into_arc_slice()
            .map_err(mem::forget)
            .ok()
            .map(Data::from_arc)
    }
    unsafe fn data_from_vec<S: Slice + ?Sized, E: AllocErrorImpl, const UNIQUE: bool>(
        vec: S::Vec,
        _offset: usize,
    ) -> Result<Data<UNIQUE>, (E, S::Vec)> {
        Ok(Data::from_arc(Arc::<S>::new_vec::<E>(vec)?))
    }

    fn clone<S: Slice + ?Sized, E: AllocErrorImpl, const UNIQUE: bool>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        data: &mut Data<UNIQUE>,
    ) -> Result<(), E> {
        mem::forget((*data.get_arc::<S, ANY_BUFFER>()).clone());
        data.make_shared();
        Ok(())
    }

    unsafe fn drop<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        _capacity: usize,
        data: Data<UNIQUE>,
    ) {
        let mut arc = ManuallyDrop::into_inner(data.get_arc::<S, ANY_BUFFER>());
        arc.set_length::<UNIQUE>(start, length);
        if data.is_unique() {
            unsafe { arc.drop_unique() };
        } else {
            drop(arc);
        }
    }

    fn get_metadata<S: Slice + ?Sized, M: Any, const UNIQUE: bool>(
        data: &Data<UNIQUE>,
    ) -> Option<&M> {
        Some(unsafe { &*ptr::from_ref((*data).get_arc::<S, ANY_BUFFER>().get_metadata()?) })
    }

    unsafe fn take_buffer<S: Slice + ?Sized, B: BufferMut<S>, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        _capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<B> {
        let arc = ManuallyDrop::into_inner(data.get_arc::<S, ANY_BUFFER>());
        unsafe { arc.take_buffer::<B, UNIQUE>(start, length) }
            .map_err(mem::forget)
            .ok()
    }

    unsafe fn take_array<T: Send + Sync + 'static, const N: usize, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        data: Data<UNIQUE>,
    ) -> Option<[T; N]> {
        let arc = ManuallyDrop::into_inner(data.get_arc::<[T], ANY_BUFFER>());
        unsafe { arc.take_array::<N, false>(start, length) }
            .map_err(mem::forget)
            .ok()
    }

    fn is_unique<S: Slice + ?Sized, const UNIQUE: bool>(data: &mut Data<UNIQUE>) -> bool {
        assert_checked(!UNIQUE);
        if data.is_unique() {
            return true;
        }
        if data.get_arc::<S, ANY_BUFFER>().is_unique() {
            data.make_unique();
            return true;
        }
        false
    }

    fn try_reserve<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        _capacity: usize,
        data: &mut Data<UNIQUE>,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<S::Item> {
        let mut arc = (*data).get_arc::<S, ANY_BUFFER>();
        let res = unsafe { arc.try_reserve::<UNIQUE>(start, length, additional, allocate) };
        if res.0.is_ok() {
            // Arc::try_reserve may reallocate the arc, but only if it succeeds, and in that case
            // the data is unique
            *data = Data(ManuallyDrop::into_inner(arc).into_raw());
        }
        res
    }

    fn frozen_data<S: Slice + ?Sized, L: ArcSliceLayout, E: AllocErrorImpl, const UNIQUE: bool>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<L::Data> {
        L::try_data_from_arc(data.get_arc::<S, ANY_BUFFER>())
    }

    fn update_layout<
        S: Slice + ?Sized,
        L: ArcSliceMutLayout,
        E: AllocErrorImpl,
        const UNIQUE: bool,
    >(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<Data<UNIQUE>> {
        L::try_data_from_arc(data.get_arc::<S, ANY_BUFFER>())
    }
}
