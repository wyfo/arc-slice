use alloc::vec::Vec;
use core::{any::Any, hint, mem, mem::ManuallyDrop, ptr::NonNull};

use crate::{
    arc::Arc, buffer::Buffer, layout::OptimizedLayout, msrv::ptr, slice::ArcSliceLayout,
    utils::static_slice,
};

impl<const ANY_BUFFER: bool, const STATIC: bool> OptimizedLayout<ANY_BUFFER, STATIC> {
    fn arc<T>(data: &<Self as ArcSliceLayout>::Data) -> Option<ManuallyDrop<Arc<T, ANY_BUFFER>>> {
        match data {
            Some(ptr) => Some(ManuallyDrop::new(unsafe { Arc::from_raw(*ptr) })),
            None if STATIC => None,
            None => unsafe { hint::unreachable_unchecked() },
        }
    }
}

impl<const ANY_BUFFER: bool, const STATIC: bool> ArcSliceLayout
    for OptimizedLayout<ANY_BUFFER, STATIC>
{
    type Data = Option<NonNull<()>>;

    const STATIC_DATA: Option<Self::Data> = None;

    fn data_from_arc<T>(arc: Arc<T>) -> Self::Data {
        Some(arc.into_raw())
    }

    fn data_from_vec<T: Send + Sync + 'static>(vec: Vec<T>) -> Self::Data {
        Some(Arc::new_vec(vec).into_raw())
    }

    fn clone<T>(_start: NonNull<T>, _length: usize, data: &Self::Data) -> Self::Data {
        Self::arc::<T>(data).map(|arc| (*arc).clone().into_raw())
    }

    unsafe fn drop<T>(
        _start: NonNull<T>,
        _length: usize,
        data: &mut ManuallyDrop<Self::Data>,
        unique_hint: bool,
    ) {
        if let Some(arc) = Self::arc::<T>(data) {
            ManuallyDrop::into_inner(arc).drop(unique_hint);
        }
    }

    fn borrowed_data<T>(data: &Self::Data) -> Option<*const ()> {
        Some(data.map_or_else(ptr::null_mut, NonNull::as_ptr))
    }

    fn clone_borrowed_data<T>(ptr: *const ()) -> Option<Self::Data> {
        let data = NonNull::new(ptr.cast_mut());
        Some(Self::arc::<T>(&data).map(|arc| (*arc).clone().into_raw()))
    }

    fn is_unique<T>(data: &Self::Data) -> bool {
        Self::arc::<T>(data).is_some_and(|arc| arc.is_unique())
    }

    fn get_metadata<T, M: Any>(data: &Self::Data) -> Option<&M> {
        Some(unsafe { &*ptr::from_ref(Self::arc::<T>(data)?.get_metadata::<M>()?) })
    }

    unsafe fn take_buffer<T: Send + Sync + 'static, B: Buffer<T>>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B> {
        let Some(arc) = Self::arc::<T>(data) else {
            return B::try_from_static(unsafe { static_slice(start, length) });
        };
        ManuallyDrop::into_inner(arc)
            .take_buffer(start, length)
            .map_err(mem::forget)
            .ok()
    }

    unsafe fn update_layout<T: Send + Sync + 'static, L: ArcSliceLayout>(
        start: NonNull<T>,
        length: usize,
        data: Self::Data,
    ) -> L::Data {
        match Self::arc::<T>(&data) {
            Some(arc) => L::data_from_arc::<T>(unsafe {
                Arc::from_raw(ManuallyDrop::into_inner(arc).into_raw())
            }),
            None => L::data_from_static(unsafe { static_slice(start, length) }),
        }
    }
}
