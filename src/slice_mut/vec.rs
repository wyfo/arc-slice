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
    utils::{assert_checked, transmute_checked, NewChecked, UnwrapInfallible},
};

const OFFSET_FLAG: usize = 0b01;
const OFFSET_SHIFT: usize = 1;

enum OffsetOrArc<S: Slice + ?Sized> {
    Arc(ManuallyDrop<Arc<S>>),
    Offset(usize),
}

impl<const UNIQUE: bool> Data<UNIQUE> {
    #[inline(always)]
    fn offset_or_arc<S: Slice + ?Sized>(&self) -> OffsetOrArc<S> {
        if self.0.addr().get() & OFFSET_FLAG != 0 {
            OffsetOrArc::Offset(self.0.addr().get() >> OFFSET_SHIFT)
        } else {
            OffsetOrArc::Arc(ManuallyDrop::new(unsafe { Arc::from_raw(self.0) }))
        }
    }
}

impl<S: Slice + ?Sized, const UNIQUE: bool> From<OffsetOrArc<S>> for Data<UNIQUE> {
    #[inline(always)]
    fn from(value: OffsetOrArc<S>) -> Self {
        Data(match value {
            OffsetOrArc::Arc(arc) => ManuallyDrop::into_inner(arc).into_raw(),
            OffsetOrArc::Offset(offset) => NonNull::new_checked(ptr::without_provenance_mut::<()>(
                OFFSET_FLAG | (offset << OFFSET_SHIFT),
            )),
        })
    }
}

unsafe fn rebuild_vec<S: Slice + ?Sized>(
    start: NonNull<S::Item>,
    length: usize,
    capacity: usize,
    offset: usize,
) -> S::Vec {
    unsafe {
        assume!(capacity + offset > 0);
        S::from_vec_unchecked(Vec::from_raw_parts(
            start.sub(offset).as_ptr(),
            length + offset,
            capacity + offset,
        ))
    }
}

impl VecLayout {}

unsafe impl ArcSliceMutLayout for VecLayout {
    const ANY_BUFFER: bool = true;

    fn try_data_from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool, const UNIQUE: bool>(
        arc: ManuallyDrop<Arc<S, ANY_BUFFER>>,
    ) -> Option<Data<UNIQUE>> {
        Some(Data(ManuallyDrop::into_inner(arc).into_raw()))
    }

    unsafe fn data_from_vec<S: Slice + ?Sized, E: AllocErrorImpl, const UNIQUE: bool>(
        vec: S::Vec,
        offset: usize,
    ) -> Result<Data<UNIQUE>, (E, S::Vec)> {
        mem::forget(vec);
        Ok(OffsetOrArc::Offset::<S>(offset).into())
    }

    fn clone<S: Slice + ?Sized, E: AllocErrorImpl, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data<UNIQUE>,
    ) -> Result<(), E> {
        match data.offset_or_arc::<S>() {
            OffsetOrArc::Arc(arc) => mem::forget((*arc).clone()),
            OffsetOrArc::Offset(offset) => {
                let vec = unsafe { rebuild_vec::<S>(start, length, capacity, offset) };
                *data = Data(Arc::from(Arc::<S>::promote_vec::<E>(vec)?).into_raw());
            }
        }
        Ok(())
    }

    unsafe fn drop<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) {
        match data.offset_or_arc::<S>() {
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
                drop(unsafe { rebuild_vec::<S>(start, length, capacity, offset) });
            }
        }
    }

    fn advance<S: Slice + ?Sized, const UNIQUE: bool>(
        data: Option<&mut Data<UNIQUE>>,
        offset: usize,
    ) {
        if let Some(data) = data {
            if let OffsetOrArc::Offset(cur_offset) = data.offset_or_arc::<S>() {
                *data = OffsetOrArc::Offset::<S>(cur_offset + offset).into();
            }
        }
    }

    fn truncate<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data<UNIQUE>,
    ) {
        if S::needs_drop() {
            if let OffsetOrArc::Offset(offset) = data.offset_or_arc::<S>() {
                let vec = unsafe { rebuild_vec::<S>(start, length, capacity, offset) };
                let arc = Arc::<S>::new_vec::<Infallible>(vec).unwrap_infallible();
                *data = Data(arc.into_raw());
            }
        }
    }

    fn get_metadata<S: Slice + ?Sized, M: Any, const UNIQUE: bool>(
        data: &Data<UNIQUE>,
    ) -> Option<&M> {
        match data.offset_or_arc::<S>() {
            OffsetOrArc::Arc(arc) => unsafe { Some(&*ptr::from_ref(arc.get_metadata::<M>()?)) },
            _ => None,
        }
    }

    unsafe fn take_buffer<S: Slice + ?Sized, B: BufferMut<S>, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<B> {
        match data.offset_or_arc::<S>() {
            OffsetOrArc::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_buffer::<B, UNIQUE>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            OffsetOrArc::Offset(offset) if is!(B, S::Vec) => {
                let mut vec = unsafe { rebuild_vec::<S>(start, length, capacity, offset) };
                if !unsafe { vec.shift_left(offset, length, S::vec_start) } {
                    return None;
                }
                Some(transmute_checked(vec))
            }
            _ => None,
        }
    }

    unsafe fn take_array<T: Send + Sync + 'static, const N: usize, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        data: Data<UNIQUE>,
    ) -> Option<[T; N]> {
        match data.offset_or_arc::<[T]>() {
            OffsetOrArc::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_array::<N, UNIQUE>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            _ => None,
        }
    }

    fn is_unique<S: Slice + ?Sized, const UNIQUE: bool>(data: &mut Data<UNIQUE>) -> bool {
        assert_checked(!UNIQUE);
        match data.offset_or_arc::<S>() {
            OffsetOrArc::Arc(mut arc) => arc.is_unique(),
            _ => true,
        }
    }

    fn try_reserve<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data<UNIQUE>,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<S::Item> {
        match data.offset_or_arc::<S>() {
            OffsetOrArc::Arc(mut arc) => unsafe {
                let res = arc.try_reserve::<UNIQUE>(start, length, additional, allocate);
                *data = OffsetOrArc::Arc(arc).into();
                res
            },
            OffsetOrArc::Offset(offset) => {
                let mut vec =
                    ManuallyDrop::new(unsafe { rebuild_vec::<S>(start, length, capacity, offset) });
                unsafe {
                    vec.try_reserve_impl(offset, length, additional, allocate, S::vec_start, || {
                        *data = OffsetOrArc::<S>::Offset(0).into();
                    })
                }
            }
        }
    }

    fn frozen_data<S: Slice + ?Sized, L: ArcSliceLayout, E: AllocErrorImpl, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<L::Data> {
        match data.offset_or_arc::<S>() {
            OffsetOrArc::Arc(arc) => L::try_data_from_arc(arc),
            OffsetOrArc::Offset(offset) if L::ANY_BUFFER => {
                let vec = unsafe { rebuild_vec::<S>(start, length, capacity, offset) };
                L::data_from_vec::<S, E>(vec).map_err(mem::forget).ok()
            }
            OffsetOrArc::Offset(_) => None,
        }
    }

    fn update_layout<
        S: Slice + ?Sized,
        L: ArcSliceMutLayout,
        E: AllocErrorImpl,
        const UNIQUE: bool,
    >(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<Data<UNIQUE>> {
        match data.offset_or_arc::<S>() {
            OffsetOrArc::Arc(arc) => L::try_data_from_arc(arc),
            _ if !L::ANY_BUFFER => None,
            OffsetOrArc::Offset(offset) => unsafe {
                let vec = rebuild_vec::<S>(start, length, capacity, offset);
                L::data_from_vec::<S, E, UNIQUE>(vec, offset)
                    .map_err(mem::forget)
                    .ok()
            },
        }
    }
}
