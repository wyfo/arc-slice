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
// #[cold]
// pub fn try_reserve_inner(
//     &mut self,
//     additional: usize,
//     allocate: bool,
// ) -> Result<(), TryReserveError> {
//     match self.inner() {
//         Inner::Vec { offset } => {
//             let mut vec = unsafe { ManuallyDrop::new(self.rebuild_vec(offset)) };
//             // `BufferMutExt::try_reclaim_or_reserve` could be used directly,
//             // but it would lead to extra work for nothing.
//             if unsafe { vec.try_reclaim(offset, self.length, additional) } {
//                 self.set_offset(0);
//                 self.start = NonNull::new(vec.as_mut_ptr()).unwrap();
//                 self.capacity = vec.capacity();
//                 return Ok(());
//             } else if !allocate {
//                 return Err(TryReserveError::Unsupported);
//             }
//             vec.reserve(additional);
//             let new_start = unsafe { vec.as_mut_ptr().add(offset) };
//             self.start = NonNull::new(new_start).unwrap();
//             self.capacity = vec.capacity() - offset;
//         }
//         Inner::Arc { mut arc, is_tail } => {
//             self.update_arc_spare_capacity(&arc, is_tail);
//             let (res, new_start) =
//                 unsafe { arc.try_reserve(additional, allocate, self.start, self.length) };
//             self.start = new_start;
//             match res {
//                 Ok(capa) => self.capacity = capa,
//                 Err(err) => return Err(err),
//             }
//         }
//     }
//     Ok(())
// }
//
// #[inline]
// pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
//     if additional <= self.spare_capacity() {
//         return Ok(());
//     }
//     self.try_reserve_inner(additional, true)
// }
//
// #[inline]
// pub fn try_extend_from_slice(&mut self, slice: &[T]) -> Result<(), TryReserveError> {
//     self.try_reserve(slice.len())?;
//     unsafe {
//         let end = self.spare_capacity_mut().as_mut_ptr().cast();
//         ptr::copy_nonoverlapping(slice.as_ptr(), end, slice.len());
//         self.set_len(self.length + slice.len());
//     }
//     Ok(())
// }

const OFFSET_FLAG: usize = 0b01;
const OFFSET_SHIFT: usize = 1;

enum OffsetOrArc<T> {
    Arc(ManuallyDrop<Arc<T>>),
    Offset(usize),
}

impl<T, const UNIQUE: bool> From<OffsetOrArc<T>> for Data<UNIQUE> {
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
    fn offset_or_arc<T, const UNIQUE: bool>(data: Data<UNIQUE>) -> OffsetOrArc<T> {
        if data.addr().get() & OFFSET_FLAG != 0 {
            OffsetOrArc::Offset(data.addr().get() >> OFFSET_SHIFT)
        } else {
            OffsetOrArc::Arc(data.into_arc::<T, true>())
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

impl ArcSliceMutLayout for VecLayout {
    #[allow(unstable_name_collisions)]
    unsafe fn data_from_vec<T: Send + Sync + 'static, const UNIQUE: bool>(
        vec: Vec<T>,
        offset: usize,
    ) -> Data<UNIQUE> {
        mem::forget(vec);
        OffsetOrArc::Offset::<T>(offset).into()
    }

    fn clone<T: Send + Sync + 'static>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data<false>,
    ) {
        match Self::offset_or_arc::<T, false>(*data) {
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
        data: Data<UNIQUE>,
    ) {
        match Self::offset_or_arc::<T, UNIQUE>(data) {
            OffsetOrArc::Arc(arc) if UNIQUE => unsafe {
                ManuallyDrop::into_inner(arc).drop_unique();
            },
            OffsetOrArc::Arc(arc) => drop(ManuallyDrop::into_inner(arc)),
            OffsetOrArc::Offset(offset) => {
                drop(unsafe { Self::rebuild_vec(start, length, capacity, offset) });
            }
        }
    }

    fn advance<T, const UNIQUE: bool>(data: Option<&mut Data<UNIQUE>>, offset: usize) {
        if let Some(data) = data {
            if let OffsetOrArc::Offset(cur_offset) = Self::offset_or_arc::<T, UNIQUE>(*data) {
                *data = OffsetOrArc::Offset::<T>(cur_offset + offset).into();
            }
        }
    }

    fn truncate<T: Send + Sync + 'static, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data<UNIQUE>,
    ) {
        if mem::needs_drop::<T>() {
            if let OffsetOrArc::Offset(offset) = Self::offset_or_arc::<T, UNIQUE>(*data) {
                let vec = unsafe { Self::rebuild_vec(start, length, capacity, offset) };
                *data = Arc::new_vec(vec).into();
            }
        }
    }

    fn get_metadata<T, M: Any, const UNIQUE: bool>(data: &Data<UNIQUE>) -> Option<&M> {
        match Self::offset_or_arc::<T, UNIQUE>(*data) {
            OffsetOrArc::Arc(arc) => unsafe { Some(&*ptr::from_ref(arc.get_metadata::<M>()?)) },
            _ => None,
        }
    }

    unsafe fn take_buffer<T: Send + Sync + 'static, const UNIQUE: bool, B: BufferMut<T>>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<B> {
        match Self::offset_or_arc::<T, UNIQUE>(data) {
            OffsetOrArc::Arc(arc) => ManuallyDrop::into_inner(arc)
                .take_buffer(start, length)
                .map_err(mem::forget)
                .ok(),
            OffsetOrArc::Offset(offset) if is!(B, Vec<T>) => {
                let mut vec = unsafe { Self::rebuild_vec(start, length, capacity, offset) };
                unsafe { vec.shift_left(offset, length) };
                transmute_checked(vec)
            }
            _ => None,
        }
    }

    fn is_unique<T, const UNIQUE: bool>(data: Data<UNIQUE>) -> bool {
        match Self::offset_or_arc::<T, UNIQUE>(data) {
            OffsetOrArc::Arc(mut arc) => arc.is_unique(),
            _ => true,
        }
    }

    fn try_reserve<T: Send + Sync + 'static, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data<UNIQUE>,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<T> {
        match Self::offset_or_arc::<T, UNIQUE>(*data) {
            OffsetOrArc::Arc(mut arc) => unsafe {
                let res = arc.try_reserve(start, length, additional, allocate);
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

    fn frozen_data<T: Send + Sync + 'static, L: ArcSliceLayout, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> L::Data {
        match Self::offset_or_arc::<T, UNIQUE>(data) {
            OffsetOrArc::Arc(arc) => L::data_from_arc(ManuallyDrop::into_inner(arc)),
            OffsetOrArc::Offset(offset) => {
                let vec = unsafe { Self::rebuild_vec(start, length, capacity, offset) };
                L::data_from_vec(vec)
            }
        }
    }

    unsafe fn update_layout<T: Send + Sync + 'static, L: ArcSliceMutLayout, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Data<UNIQUE> {
        match Self::offset_or_arc::<T, UNIQUE>(data) {
            OffsetOrArc::Offset(offset) => unsafe {
                let vec = Self::rebuild_vec(start, length, capacity, offset);
                L::data_from_vec(vec, offset)
            },
            _ => data,
        }
    }
}
