use alloc::vec::Vec;
use core::{any::Any, mem, mem::ManuallyDrop, ptr::NonNull};

use crate::{
    arc::Arc,
    buffer::BufferMut,
    layout::ArcLayout,
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut::{ArcSliceMutLayout, Data, TryReserveResult},
};

unsafe impl<const ANY_BUFFER: bool, const STATIC: bool> ArcSliceMutLayout
    for ArcLayout<ANY_BUFFER, STATIC>
{
    unsafe fn data_from_vec<T: Send + Sync + 'static>(vec: Vec<T>, _offset: usize) -> Data {
        Arc::new_vec(vec).into_raw().into()
    }

    fn clone<T: Send + Sync + 'static>(
        _start: NonNull<T>,
        _length: usize,
        _capacity: usize,
        data: &mut Data,
    ) {
        mem::forget((*data.into_arc::<T, ANY_BUFFER>()).clone());
    }

    unsafe fn drop<T: Send + Sync + 'static, const UNIQUE: bool>(
        _start: NonNull<T>,
        _length: usize,
        _capacity: usize,
        data: Data,
    ) {
        let arc = ManuallyDrop::into_inner(data.into_arc::<T, ANY_BUFFER>());
        if UNIQUE {
            unsafe { arc.drop_unique() };
        } else {
            drop(arc);
        }
    }

    fn get_metadata<T, M: Any>(data: &Data) -> Option<&M> {
        Some(unsafe { &*ptr::from_ref((*data).into_arc::<T, ANY_BUFFER>().get_metadata()?) })
    }

    unsafe fn take_buffer<T: Send + Sync + 'static, B: BufferMut<T>, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        _capacity: usize,
        data: Data,
    ) -> Option<B> {
        let arc = ManuallyDrop::into_inner(data.into_arc::<T, ANY_BUFFER>());
        unsafe { arc.take_buffer::<B, UNIQUE>(start, length) }
            .map_err(mem::forget)
            .ok()
    }

    fn is_unique<T>(data: Data) -> bool {
        data.into_arc::<T, ANY_BUFFER>().is_unique()
    }

    fn try_reserve<T: Send + Sync + 'static, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        _capacity: usize,
        data: &mut Data,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<T> {
        let mut arc = (*data).into_arc::<T, ANY_BUFFER>();
        let res = unsafe { arc.try_reserve::<UNIQUE>(start, length, additional, allocate) };
        *data = ManuallyDrop::into_inner(arc).into();
        res
    }

    fn frozen_data<T: Send + Sync + 'static, L: ArcSliceLayout>(
        _start: NonNull<T>,
        _length: usize,
        _capacity: usize,
        data: Data,
    ) -> L::Data {
        L::data_from_arc(ManuallyDrop::into_inner(data.into_arc::<T, ANY_BUFFER>()))
    }
}
