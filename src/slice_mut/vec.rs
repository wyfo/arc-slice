use alloc::vec::Vec;
use core::{any::Any, mem, mem::ManuallyDrop, ptr::NonNull};

#[allow(unused_imports)]
use crate::msrv::{NonNullExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{BufferMut, BufferMutExt},
    layout::VecLayout,
    macros::{assume, is},
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut::{ArcSliceMutLayout, Data, TryReserveResult},
    utils::{transmute_checked, NewChecked},
};

const OFFSET_FLAG: usize = 0b01;
const OFFSET_SHIFT: usize = 1;

enum OffsetOrArc<T> {
    Arc(ManuallyDrop<Arc<T>>),
    Offset(usize),
}

impl<T> From<OffsetOrArc<T>> for Data {
    #[inline(always)]
    fn from(value: OffsetOrArc<T>) -> Self {
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
    fn offset_or_arc<T>(data: Data) -> OffsetOrArc<T> {
        if data.addr().get() & OFFSET_FLAG != 0 {
            OffsetOrArc::Offset(data.addr().get() >> OFFSET_SHIFT)
        } else {
            OffsetOrArc::Arc(data.into_arc())
        }
    }

    unsafe fn rebuild_vec<T>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        offset: usize,
    ) -> Vec<T> {
        unsafe {
            assume!(capacity > 0);
            Vec::from_raw_parts(
                start.sub(offset).as_ptr(),
                length + offset,
                capacity + offset,
            )
        }
    }
}

unsafe impl ArcSliceMutLayout for VecLayout {
    unsafe fn data_from_vec<T: Send + Sync + 'static>(vec: Vec<T>, offset: usize) -> Data {
        mem::forget(vec);
        OffsetOrArc::Offset::<T>(offset).into()
    }

    fn clone<T: Send + Sync + 'static>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data,
    ) {
        match Self::offset_or_arc::<T>(*data) {
            OffsetOrArc::Arc(arc) => mem::forget((*arc).clone()),
            OffsetOrArc::Offset(offset) => {
                let vec = unsafe { Self::rebuild_vec(start, length, capacity, offset) };
                *data = Arc::from(Arc::promote_vec(vec)).into();
            }
        }
    }

    unsafe fn drop<T: Send + Sync + 'static, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data,
    ) {
        match Self::offset_or_arc::<T>(data) {
            OffsetOrArc::Arc(arc) if UNIQUE => unsafe {
                ManuallyDrop::into_inner(arc).drop_unique();
            },
            OffsetOrArc::Arc(arc) => drop(ManuallyDrop::into_inner(arc)),
            OffsetOrArc::Offset(offset) => {
                drop(unsafe { Self::rebuild_vec(start, length, capacity, offset) });
            }
        }
    }

    fn advance<T>(data: Option<&mut Data>, offset: usize) {
        if let Some(data) = data {
            if let OffsetOrArc::Offset(cur_offset) = Self::offset_or_arc::<T>(*data) {
                *data = OffsetOrArc::Offset::<T>(cur_offset + offset).into();
            }
        }
    }

    fn truncate<T: Send + Sync + 'static>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data,
    ) {
        if mem::needs_drop::<T>() {
            if let OffsetOrArc::Offset(offset) = Self::offset_or_arc::<T>(*data) {
                let vec = unsafe { Self::rebuild_vec(start, length, capacity, offset) };
                *data = Arc::new_vec(vec).into();
            }
        }
    }

    fn get_metadata<T, M: Any>(data: &Data) -> Option<&M> {
        match Self::offset_or_arc::<T>(*data) {
            OffsetOrArc::Arc(arc) => unsafe { Some(&*ptr::from_ref(arc.get_metadata::<M>()?)) },
            _ => None,
        }
    }

    unsafe fn take_buffer<T: Send + Sync + 'static, B: BufferMut<T>, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Option<B> {
        match Self::offset_or_arc::<T>(data) {
            OffsetOrArc::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_buffer::<B, UNIQUE>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            OffsetOrArc::Offset(offset) if is!(B, Vec<T>) => {
                let mut vec = unsafe { Self::rebuild_vec(start, length, capacity, offset) };
                unsafe { vec.shift_left(offset, length) };
                transmute_checked(vec)
            }
            _ => None,
        }
    }

    fn is_unique<T>(data: Data) -> bool {
        match Self::offset_or_arc::<T>(data) {
            OffsetOrArc::Arc(mut arc) => arc.is_unique(),
            _ => true,
        }
    }

    fn try_reserve<T: Send + Sync + 'static, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<T> {
        match Self::offset_or_arc::<T>(*data) {
            OffsetOrArc::Arc(mut arc) => unsafe {
                let res = arc.try_reserve::<UNIQUE>(start, length, additional, allocate);
                *data = ManuallyDrop::into_inner(arc).into();
                res
            },
            OffsetOrArc::Offset(offset) => {
                let mut vec = ManuallyDrop::new(unsafe {
                    Self::rebuild_vec(start, length, capacity, offset)
                });
                unsafe { vec.try_reserve_impl(offset, length, additional, allocate) }
            }
        }
    }

    fn frozen_data<T: Send + Sync + 'static, L: ArcSliceLayout>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> L::Data {
        match Self::offset_or_arc::<T>(data) {
            OffsetOrArc::Arc(arc) => L::data_from_arc(ManuallyDrop::into_inner(arc)),
            OffsetOrArc::Offset(offset) => {
                let vec = unsafe { Self::rebuild_vec(start, length, capacity, offset) };
                L::data_from_vec(vec)
            }
        }
    }

    unsafe fn update_layout<T: Send + Sync + 'static, L: ArcSliceMutLayout>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Data {
        match Self::offset_or_arc::<T>(data) {
            OffsetOrArc::Offset(offset) => unsafe {
                let vec = Self::rebuild_vec(start, length, capacity, offset);
                L::data_from_vec(vec, offset)
            },
            _ => data,
        }
    }
}
