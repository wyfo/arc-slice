use alloc::vec::Vec;
use core::{any::Any, convert::Infallible, mem, mem::ManuallyDrop, ptr::NonNull};

#[allow(unused_imports)]
use crate::msrv::{NonNullExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{BufferMut, BufferMutExt, Slice, SliceExt},
    error::AllocErrorImpl,
    layout::VecLayout,
    macros::{assume, is},
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut::{ArcSliceMutLayout, Data, TryReserveResult},
    utils::{transmute_checked, NewChecked, UnwrapChecked},
};

const OFFSET_FLAG: usize = 0b01;
const OFFSET_SHIFT: usize = 1;

enum OffsetOrArc<S: Slice + ?Sized> {
    Arc(ManuallyDrop<Arc<S>>),
    Offset(usize),
}

impl<S: Slice + ?Sized> From<OffsetOrArc<S>> for Data {
    #[inline(always)]
    fn from(value: OffsetOrArc<S>) -> Self {
        match value {
            OffsetOrArc::Arc(arc) => ManuallyDrop::into_inner(arc).into(),
            OffsetOrArc::Offset(offset) => NonNull::new_checked(ptr::without_provenance_mut::<()>(
                OFFSET_FLAG | (offset << OFFSET_SHIFT),
            ))
            .into(),
        }
    }
}

impl VecLayout {
    #[inline(always)]
    fn offset_or_arc<S: Slice + ?Sized>(data: Data) -> OffsetOrArc<S> {
        if data.addr().get() & OFFSET_FLAG != 0 {
            OffsetOrArc::Offset(data.addr().get() >> OFFSET_SHIFT)
        } else {
            OffsetOrArc::Arc(data.into_arc())
        }
    }

    unsafe fn rebuild_vec<S: Slice + ?Sized>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        offset: usize,
    ) -> S::Vec {
        unsafe {
            assume!(capacity > 0);
            S::from_vec_unchecked(Vec::from_raw_parts(
                start.sub(offset).as_ptr(),
                length + offset,
                capacity + offset,
            ))
        }
    }
}

unsafe impl ArcSliceMutLayout for VecLayout {
    const ANY_BUFFER: bool = true;

    unsafe fn data_from_vec<S: Slice + ?Sized, E: AllocErrorImpl>(
        vec: S::Vec,
        offset: usize,
    ) -> Result<Data, (E, S::Vec)> {
        mem::forget(vec);
        Ok(OffsetOrArc::Offset::<S>(offset).into())
    }

    fn clone<S: Slice + ?Sized, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data,
    ) -> Result<(), E> {
        match Self::offset_or_arc::<S>(*data) {
            OffsetOrArc::Arc(arc) => mem::forget((*arc).clone()),
            OffsetOrArc::Offset(offset) => {
                let vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, offset) };
                *data = Arc::from(Arc::<S>::promote_vec::<E>(vec)?).into();
            }
        }
        Ok(())
    }

    unsafe fn drop<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data,
    ) {
        match Self::offset_or_arc::<S>(data) {
            OffsetOrArc::Arc(arc) => {
                let mut arc = ManuallyDrop::into_inner(arc);
                arc.set_length::<UNIQUE>(start, length);
                if UNIQUE {
                    unsafe { arc.drop_unique() };
                } else {
                    drop(arc);
                }
            }
            OffsetOrArc::Offset(offset) => {
                drop(unsafe { Self::rebuild_vec::<S>(start, length, capacity, offset) });
            }
        }
    }

    fn advance<S: Slice + ?Sized>(data: Option<&mut Data>, offset: usize) {
        if let Some(data) = data {
            if let OffsetOrArc::Offset(cur_offset) = Self::offset_or_arc::<S>(*data) {
                *data = OffsetOrArc::Offset::<S>(cur_offset + offset).into();
            }
        }
    }

    fn truncate<S: Slice + ?Sized>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data,
    ) {
        if S::needs_drop() {
            if let OffsetOrArc::Offset(offset) = Self::offset_or_arc::<S>(*data) {
                let vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, offset) };
                *data = Arc::<S>::new_vec::<Infallible>(vec).unwrap_checked().into();
            }
        }
    }

    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Data) -> Option<&M> {
        match Self::offset_or_arc::<S>(*data) {
            OffsetOrArc::Arc(arc) => unsafe { Some(&*ptr::from_ref(arc.get_metadata::<M>()?)) },
            _ => None,
        }
    }

    unsafe fn take_buffer<S: Slice + ?Sized, B: BufferMut<S>, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Option<B> {
        match Self::offset_or_arc::<S>(data) {
            OffsetOrArc::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_buffer::<B, UNIQUE>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            OffsetOrArc::Offset(offset) if is!(B, S::Vec) => {
                let mut vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, offset) };
                unsafe { vec.shift_left(offset, length, S::vec_start) };
                transmute_checked(vec)
            }
            _ => None,
        }
    }

    unsafe fn take_array<T: Send + Sync + 'static, const N: usize, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        data: Data,
    ) -> Option<[T; N]> {
        match Self::offset_or_arc::<[T]>(data) {
            OffsetOrArc::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_array::<N, UNIQUE>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            _ => None,
        }
    }

    fn is_unique<S: Slice + ?Sized>(data: Data) -> bool {
        match Self::offset_or_arc::<S>(data) {
            OffsetOrArc::Arc(mut arc) => arc.is_unique(),
            _ => true,
        }
    }

    fn try_reserve<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<S::Item> {
        match Self::offset_or_arc::<S>(*data) {
            OffsetOrArc::Arc(mut arc) => unsafe {
                let res = arc.try_reserve::<UNIQUE>(start, length, additional, allocate);
                *data = ManuallyDrop::into_inner(arc).into();
                res
            },
            OffsetOrArc::Offset(offset) => {
                let mut vec = ManuallyDrop::new(unsafe {
                    Self::rebuild_vec::<S>(start, length, capacity, offset)
                });
                unsafe { vec.try_reserve_impl(offset, length, additional, allocate, S::vec_start) }
            }
        }
    }

    fn frozen_data<S: Slice + ?Sized, L: ArcSliceLayout, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Option<L::Data> {
        match Self::offset_or_arc::<S>(data) {
            OffsetOrArc::Arc(arc) => L::try_data_from_arc(arc),
            OffsetOrArc::Offset(offset) if L::ANY_BUFFER => {
                let vec = unsafe { Self::rebuild_vec::<S>(start, length, capacity, offset) };
                L::data_from_vec::<S, E>(vec).map_err(mem::forget).ok()
            }
            OffsetOrArc::Offset(_) => None,
        }
    }

    fn update_layout<S: Slice + ?Sized, L: ArcSliceMutLayout, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Option<Data> {
        match Self::offset_or_arc::<S>(data) {
            OffsetOrArc::Arc(arc) => L::try_data_from_arc(arc),
            _ if !L::ANY_BUFFER => None,
            OffsetOrArc::Offset(offset) => unsafe {
                let vec = Self::rebuild_vec::<S>(start, length, capacity, offset);
                L::data_from_vec::<S, E>(vec, offset)
                    .map_err(mem::forget)
                    .ok()
            },
        }
    }
}
