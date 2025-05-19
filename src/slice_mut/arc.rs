use core::{any::Any, mem, mem::ManuallyDrop, ptr::NonNull};

use crate::{
    arc::Arc,
    buffer::{BufferMut, Slice},
    layout::ArcLayout,
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut::{ArcSliceMutLayout, Data, TryReserveResult},
};

unsafe impl<const ANY_BUFFER: bool, const STATIC: bool> ArcSliceMutLayout
    for ArcLayout<ANY_BUFFER, STATIC>
{
    unsafe fn data_from_vec<S: Slice + ?Sized>(vec: S::Vec, _offset: usize) -> Data {
        Arc::<S>::new_vec(vec).into_raw().into()
    }

    fn clone<S: Slice + ?Sized>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        data: &mut Data,
    ) {
        mem::forget((*data.into_arc::<S, ANY_BUFFER>()).clone());
    }

    unsafe fn drop<S: Slice + ?Sized, const UNIQUE: bool>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        data: Data,
    ) {
        let arc = ManuallyDrop::into_inner(data.into_arc::<S, ANY_BUFFER>());
        if UNIQUE {
            unsafe { arc.drop_unique() };
        } else {
            drop(arc);
        }
    }

    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Data) -> Option<&M> {
        Some(unsafe { &*ptr::from_ref((*data).into_arc::<S, ANY_BUFFER>().get_metadata()?) })
    }

    unsafe fn take_buffer<S: Slice + ?Sized, B: BufferMut<S>, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        _capacity: usize,
        data: Data,
    ) -> Option<B> {
        let arc = ManuallyDrop::into_inner(data.into_arc::<S, ANY_BUFFER>());
        unsafe { arc.take_buffer::<B, UNIQUE>(start, length) }
            .map_err(mem::forget)
            .ok()
    }

    unsafe fn take_array<T: Send + Sync + 'static, const N: usize, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        data: Data,
    ) -> Option<[T; N]> {
        let arc = ManuallyDrop::into_inner(data.into_arc::<[T], ANY_BUFFER>());
        unsafe { arc.take_array::<N, false>(start, length) }
            .map_err(mem::forget)
            .ok()
    }

    fn is_unique<S: Slice + ?Sized>(data: Data) -> bool {
        data.into_arc::<S, ANY_BUFFER>().is_unique()
    }

    fn try_reserve<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        _capacity: usize,
        data: &mut Data,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<S::Item> {
        let mut arc = (*data).into_arc::<S, ANY_BUFFER>();
        let res = unsafe { arc.try_reserve::<UNIQUE>(start, length, additional, allocate) };
        *data = ManuallyDrop::into_inner(arc).into();
        res
    }

    fn frozen_data<S: Slice + ?Sized, L: ArcSliceLayout>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        data: Data,
    ) -> L::Data {
        L::data_from_arc(ManuallyDrop::into_inner(data.into_arc::<S, ANY_BUFFER>()))
    }
}
