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
    buffer::{Buffer, BufferWithMetadata, Slice, SliceExt},
    layout::ArcLayout,
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut,
    slice_mut::ArcSliceMutLayout,
    utils::{assert_checked, try_transmute},
};

impl<const ANY_BUFFER: bool, const STATIC: bool> ArcLayout<ANY_BUFFER, STATIC> {
    fn arc<S: Slice + ?Sized>(
        data: &<Self as ArcSliceLayout>::Data,
    ) -> Option<ManuallyDrop<Arc<S, ANY_BUFFER>>> {
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

    const ANY_BUFFER: bool = ANY_BUFFER;
    const STATIC_DATA: Option<Self::Data> = if STATIC { Some(None) } else { None };
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> = MaybeUninit::new(None);

    fn data_from_arc<S: Slice + ?Sized, const ANY_BUFFER2: bool>(
        arc: Arc<S, ANY_BUFFER2>,
    ) -> Self::Data {
        assert_checked(ANY_BUFFER || !ANY_BUFFER2);
        Some(arc.into_raw())
    }

    fn data_from_static<S: Slice + ?Sized>(slice: &'static S) -> Self::Data {
        if let Some(data) = Self::STATIC_DATA {
            return data;
        }
        assert_checked(ANY_BUFFER);
        Self::data_from_arc(Arc::new_buffer(BufferWithMetadata::new(slice, ())).0)
    }

    fn data_from_vec<S: Slice + ?Sized>(vec: S::Vec) -> Self::Data {
        assert_checked(ANY_BUFFER);
        Some(Arc::<S>::new_vec(vec).into_raw())
    }

    fn clone<S: Slice + ?Sized>(
        _start: NonNull<S::Item>,
        _length: usize,
        data: &Self::Data,
    ) -> Self::Data {
        Some((*Self::arc::<S>(data)?).clone().into_raw())
    }

    unsafe fn drop<S: Slice + ?Sized, const UNIQUE_HINT: bool>(
        _start: NonNull<S::Item>,
        _length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) {
        if let Some(arc) = Self::arc::<S>(data) {
            ManuallyDrop::into_inner(arc).drop_with_unique_hint::<UNIQUE_HINT>();
        }
    }

    fn borrowed_data<S: Slice + ?Sized>(data: &Self::Data) -> Option<*const ()> {
        Some(data.map_or_else(ptr::null_mut, NonNull::as_ptr))
    }

    fn clone_borrowed_data<S: Slice + ?Sized>(ptr: *const ()) -> Option<Self::Data> {
        let data = NonNull::new(ptr.cast_mut());
        Some(Self::arc::<S>(&data).map(|arc| (*arc).clone().into_raw()))
    }

    fn is_unique<S: Slice + ?Sized>(data: &Self::Data) -> bool {
        Self::arc::<S>(data).is_some_and(|arc| arc.is_buffer_unique())
    }

    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Self::Data) -> Option<&M> {
        Some(unsafe { &*ptr::from_ref(Self::arc::<S>(data)?.get_metadata::<M>()?) })
    }

    unsafe fn take_buffer<S: Slice + ?Sized, B: Buffer<S>>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B> {
        match Self::arc::<S>(data) {
            Some(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_buffer::<B, false>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            None => try_transmute(unsafe { S::from_raw_parts::<'static>(start, length) }).ok(),
        }
    }

    unsafe fn take_array<T: Send + Sync + 'static, const N: usize>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<[T; N]> {
        let arc = ManuallyDrop::into_inner(Self::arc::<[T]>(data)?);
        unsafe { arc.take_array::<N, false>(start, length) }
            .map_err(mem::forget)
            .ok()
    }

    unsafe fn mut_data<S: Slice + ?Sized, L: ArcSliceMutLayout>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<slice_mut::Data>)> {
        match Self::arc::<S>(data) {
            Some(mut arc) => Some((
                unsafe { arc.capacity(start)? },
                Some(ManuallyDrop::into_inner(arc).into()),
            )),
            None => (length == 0).then_some((0, None)),
        }
    }

    unsafe fn update_layout<S: Slice + ?Sized, L: ArcSliceLayout>(
        start: NonNull<S::Item>,
        length: usize,
        data: Self::Data,
    ) -> L::Data {
        match Self::arc::<S>(&data) {
            Some(arc) => L::data_from_arc(ManuallyDrop::into_inner(arc)),
            None => L::data_from_static(unsafe { S::from_raw_parts(start, length) }),
        }
    }
}
