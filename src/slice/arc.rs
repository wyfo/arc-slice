use alloc::vec::Vec;
use core::{
    any::Any,
    hint, mem,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::NonNull,
};

#[allow(unused_imports)]
use crate::msrv::{BoolExt, ConstPtrExt, OptionExt};
use crate::{
    arc::Arc,
    buffer::{Buffer, BufferWithMetadata},
    layout::ArcLayout,
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut,
    slice_mut::ArcSliceMutLayout,
    utils::{static_slice, try_transmute},
};

impl<const ANY_BUFFER: bool, const STATIC: bool> ArcLayout<ANY_BUFFER, STATIC> {
    fn arc<T>(data: &<Self as ArcSliceLayout>::Data) -> Option<ManuallyDrop<Arc<T, ANY_BUFFER>>> {
        match data {
            Some(ptr) => Some(ManuallyDrop::new(unsafe { Arc::from_raw(*ptr) })),
            None if STATIC => None,
            None => unsafe { hint::unreachable_unchecked() },
        }
    }
}

unsafe impl<const ANY_BUFFER: bool, const STATIC: bool> ArcSliceLayout
    for ArcLayout<ANY_BUFFER, STATIC>
{
    type Data = Option<NonNull<()>>;

    const STATIC_DATA: Option<Self::Data> = if STATIC { Some(None) } else { None };
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> = MaybeUninit::new(None);

    fn data_from_arc<T, const ANY_BUFFER2: bool>(arc: Arc<T, ANY_BUFFER2>) -> Self::Data {
        Some(arc.into_raw())
    }

    fn data_from_static<T: Send + Sync + 'static>(slice: &'static [T]) -> Self::Data {
        if let Some(data) = Self::STATIC_DATA {
            return data;
        }
        assert!(ANY_BUFFER);
        Self::data_from_arc(Arc::new_buffer(BufferWithMetadata::new(slice, ())).0)
    }

    fn data_from_vec<T: Send + Sync + 'static>(vec: Vec<T>) -> Self::Data {
        Some(Arc::new_vec(vec).into_raw())
    }

    fn clone<T>(_start: NonNull<T>, _length: usize, data: &Self::Data) -> Self::Data {
        Some((*Self::arc::<T>(data)?).clone().into_raw())
    }

    unsafe fn drop<T, const UNIQUE_HINT: bool>(
        _start: NonNull<T>,
        _length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) {
        if let Some(arc) = Self::arc::<T>(data) {
            ManuallyDrop::into_inner(arc).drop_with_unique_hint::<UNIQUE_HINT>();
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
        Self::arc::<T>(data).is_some_and(|arc| arc.is_buffer_unique())
    }

    fn get_metadata<T, M: Any>(data: &Self::Data) -> Option<&M> {
        Some(unsafe { &*ptr::from_ref(Self::arc::<T>(data)?.get_metadata::<M>()?) })
    }

    unsafe fn take_buffer<T: Send + Sync + 'static, B: Buffer<T>>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B> {
        match Self::arc::<T>(data) {
            Some(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_buffer::<B, false>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            None => try_transmute(unsafe { static_slice(start, length) }).ok(),
        }
    }

    unsafe fn mut_data<T: Send + Sync + 'static, L: ArcSliceMutLayout>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<slice_mut::Data>)> {
        match Self::arc::<T>(data) {
            Some(mut arc) => Some((
                unsafe { arc.capacity(start)? },
                Some(ManuallyDrop::into_inner(arc).into()),
            )),
            None => (length == 0).then_some((0, None)),
        }
    }

    unsafe fn update_layout<T: Send + Sync + 'static, L: ArcSliceLayout>(
        start: NonNull<T>,
        length: usize,
        data: Self::Data,
    ) -> L::Data {
        match Self::arc::<T>(&data) {
            Some(arc) => L::data_from_arc(ManuallyDrop::into_inner(arc)),
            None => L::data_from_static(unsafe { static_slice(start, length) }),
        }
    }
}
