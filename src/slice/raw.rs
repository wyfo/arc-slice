use alloc::vec::Vec;
use core::{
    any::{Any, TypeId},
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::NonNull,
};

#[allow(unused_imports)]
use crate::msrv::{ConstPtrExt, NonNullExt};
use crate::{
    arc::{vtable as arc_vtable, Arc},
    buffer::{Buffer, DynBuffer, RawBuffer},
    layout::RawLayout,
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut,
    slice_mut::ArcSliceMutLayout,
    utils::static_slice,
    vtable::{generic_take_buffer, VTable},
};

mod static_vtable {
    use core::{any::TypeId, ptr::NonNull};

    #[allow(unused_imports)]
    use crate::msrv::NonNullExt;
    use crate::{
        macros::is_not,
        utils::static_slice,
        vtable::{no_capacity, VTable},
    };

    unsafe fn deallocate(_ptr: *mut ()) {}
    unsafe fn is_buffer_unique(_ptr: *const ()) -> bool {
        false
    }
    unsafe fn get_metadata(_ptr: *const (), _type_id: TypeId) -> Option<NonNull<()>> {
        None
    }
    unsafe fn take_buffer<T: 'static>(
        buffer: NonNull<()>,
        _ptr: *const (),
        type_id: TypeId,
        start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>> {
        if is_not!({ type_id }, &'static [T]) {
            return None;
        }
        unsafe { buffer.cast().write(static_slice(start.cast::<T>(), length)) };
        Some(buffer)
    }
    unsafe fn drop(_ptr: *const ()) {}
    unsafe fn drop_with_unique_hint(_ptr: *const ()) {}
    unsafe fn clone(_ptr: *const ()) {}
    unsafe fn into_arc(_ptr: *const ()) -> Option<NonNull<()>> {
        None
    }

    pub(super) const fn new_vtable<T: 'static>() -> &'static VTable {
        &VTable {
            deallocate,
            drop,
            drop_with_unique_hint,
            clone,
            is_buffer_unique,
            get_metadata,
            take_buffer: take_buffer::<T>,
            capacity: no_capacity,
            try_reserve: None,
            into_arc,
        }
    }
}

mod raw_vtable {
    use core::{any::TypeId, mem, mem::ManuallyDrop, ptr::NonNull};

    #[allow(unused_imports)]
    use crate::msrv::NonNullExt;
    use crate::{
        arc::Arc,
        buffer::{DynBuffer, RawBuffer},
        macros::{is, is_not},
        vtable::{no_capacity, VTable},
    };

    unsafe fn deallocate(_ptr: *mut ()) {
        unreachable!()
    }

    unsafe fn is_buffer_unique<T, B: RawBuffer<T>>(ptr: *const ()) -> bool {
        ManuallyDrop::new(unsafe { B::from_raw(ptr) }).is_unique()
    }

    unsafe fn get_metadata<T, B: DynBuffer + RawBuffer<T>>(
        ptr: *const (),
        type_id: TypeId,
    ) -> Option<NonNull<()>> {
        if is!(B::Metadata, ()) || is_not!({ type_id }, B::Metadata) {
            return None;
        }
        Some(NonNull::from(ManuallyDrop::new(unsafe { B::from_raw(ptr) }).get_metadata()).cast())
    }

    unsafe fn take_buffer<T, B: DynBuffer + RawBuffer<T>>(
        buffer: NonNull<()>,
        ptr: *const (),
        type_id: TypeId,
        _start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>> {
        let raw_buffer = ManuallyDrop::new(unsafe { B::from_raw(ptr) });
        if is_not!({ type_id }, B::Buffer) || raw_buffer.as_slice().len() != length {
            return None;
        }
        unsafe { buffer.cast().write(ManuallyDrop::into_inner(raw_buffer)) };
        Some(buffer)
    }

    unsafe fn drop<T, B: RawBuffer<T>>(ptr: *const ()) {
        mem::drop(unsafe { B::from_raw(ptr) });
    }
    unsafe fn clone<T, B: RawBuffer<T>>(ptr: *const ()) {
        let _ = (*ManuallyDrop::new(unsafe { B::from_raw(ptr) })).clone();
    }

    unsafe fn into_arc<T, B: DynBuffer + RawBuffer<T>>(ptr: *const ()) -> Option<NonNull<()>> {
        Some(Arc::new_buffer(unsafe { B::from_raw(ptr) }).0.into_raw())
    }

    pub(super) const fn new_vtable<T, B: DynBuffer + RawBuffer<T>>() -> &'static VTable {
        &VTable {
            deallocate,
            drop: drop::<T, B>,
            drop_with_unique_hint: drop::<T, B>,
            clone: clone::<T, B>,
            is_buffer_unique: is_buffer_unique::<T, B>,
            get_metadata: get_metadata::<T, B>,
            take_buffer: take_buffer::<T, B>,
            capacity: no_capacity,
            try_reserve: None,
            into_arc: into_arc::<T, B>,
        }
    }
}

enum ArcOrVTable<T> {
    Arc(ManuallyDrop<Arc<T, false>>),
    Vtable {
        ptr: *const (),
        vtable: &'static VTable,
    },
}

fn arc_or_vtable<T>((ptr, vtable): <RawLayout as ArcSliceLayout>::Data) -> ArcOrVTable<T> {
    match vtable {
        Some(vtable) => ArcOrVTable::Vtable { ptr, vtable },
        None => ArcOrVTable::Arc(ManuallyDrop::new(unsafe {
            Arc::from_raw(NonNull::new_unchecked(ptr.cast_mut()))
        })),
    }
}

unsafe impl ArcSliceLayout for RawLayout {
    type Data = (*const (), Option<&'static VTable>);
    const STATIC_DATA: Option<Self::Data> =
        Some((ptr::null(), Some(static_vtable::new_vtable::<()>())));
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> =
        MaybeUninit::new((ptr::null(), Some(static_vtable::new_vtable::<()>())));

    fn data_from_arc<T, const ANY_BUFFER: bool>(arc: Arc<T, ANY_BUFFER>) -> Self::Data {
        let vtable = arc.vtable();
        (arc.into_raw().as_ptr(), vtable)
    }

    fn data_from_arc_slice<T, const ANY_BUFFER: bool>(arc: Arc<T, ANY_BUFFER>) -> Self::Data {
        (arc.into_raw().as_ptr(), None)
    }

    fn data_from_arc_buffer<T, const ANY_BUFFER: bool, B: DynBuffer + Buffer<T>>(
        arc: Arc<T, ANY_BUFFER>,
    ) -> Self::Data {
        (arc.into_raw().as_ptr(), Some(arc_vtable::new::<T, B>()))
    }

    fn data_from_static<T: Send + Sync + 'static>(_slice: &'static [T]) -> Self::Data {
        (ptr::null(), Some(static_vtable::new_vtable::<T>()))
    }

    fn data_from_vec<T: Send + Sync + 'static>(vec: Vec<T>) -> Self::Data {
        (
            Arc::new_vec(vec).into_raw().as_ptr(),
            Some(arc_vtable::new_vec::<T>()),
        )
    }

    fn data_from_raw_buffer<T, B: DynBuffer + RawBuffer<T>>(
        buffer: *const (),
    ) -> Option<Self::Data> {
        Some((buffer, Some(raw_vtable::new_vtable::<T, B>())))
    }

    fn clone<T: Send + Sync + 'static>(
        _start: NonNull<T>,
        _length: usize,
        data: &Self::Data,
    ) -> Self::Data {
        match arc_or_vtable::<T>(*data) {
            ArcOrVTable::Arc(arc) => mem::forget((*arc).clone()),
            ArcOrVTable::Vtable { ptr, vtable } => unsafe { (vtable.clone)(ptr) },
        }
        *data
    }

    unsafe fn drop<T, const UNIQUE_HINT: bool>(
        _start: NonNull<T>,
        _length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) {
        match arc_or_vtable::<T>(**data) {
            ArcOrVTable::Arc(arc) => {
                ManuallyDrop::into_inner(arc).drop_with_unique_hint::<UNIQUE_HINT>();
            }
            ArcOrVTable::Vtable { ptr, vtable } if UNIQUE_HINT => unsafe {
                (vtable.drop_with_unique_hint)(ptr);
            },
            ArcOrVTable::Vtable { ptr, vtable } => unsafe { (vtable.drop)(ptr) },
        }
    }

    fn is_unique<T>(data: &Self::Data) -> bool {
        match arc_or_vtable::<T>(*data) {
            ArcOrVTable::Arc(arc) => arc.is_buffer_unique(),
            ArcOrVTable::Vtable { ptr, vtable } => unsafe { (vtable.is_buffer_unique)(ptr) },
        }
    }

    fn get_metadata<T, M: Any>(data: &Self::Data) -> Option<&M> {
        match arc_or_vtable::<T>(*data) {
            ArcOrVTable::Arc(arc) => Some(unsafe { &*ptr::from_ref(arc.get_metadata::<M>()?) }),
            ArcOrVTable::Vtable { ptr, vtable } => unsafe {
                let metadata = (vtable.get_metadata)(ptr, TypeId::of::<M>())?;
                Some(metadata.cast().as_ref())
            },
        }
    }

    unsafe fn take_buffer<T: Send + Sync + 'static, B: Buffer<T>>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B> {
        match arc_or_vtable::<T>(**data) {
            ArcOrVTable::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_buffer::<B, false>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            ArcOrVTable::Vtable { ptr, vtable } => unsafe {
                generic_take_buffer(ptr, vtable, start.cast(), length)
            },
        }
    }

    unsafe fn mut_data<T: Send + Sync + 'static, L: ArcSliceMutLayout>(
        start: NonNull<T>,
        _length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<slice_mut::Data>)> {
        match arc_or_vtable::<T>(**data) {
            ArcOrVTable::Arc(mut arc) => Some((
                unsafe { arc.capacity(start)? },
                Some(ManuallyDrop::into_inner(arc).into()),
            )),
            ArcOrVTable::Vtable { ptr, vtable } => {
                let capacity = unsafe { (vtable.capacity)(ptr, start.cast()) };
                (capacity != usize::MAX).then(|| {
                    let data = unsafe { NonNull::new_unchecked(ptr.cast_mut()) }.into();
                    (capacity, Some(data))
                })
            }
        }
    }

    unsafe fn update_layout<T: Send + Sync + 'static, L: ArcSliceLayout>(
        start: NonNull<T>,
        length: usize,
        data: Self::Data,
    ) -> L::Data {
        match arc_or_vtable::<T>(data) {
            ArcOrVTable::Arc(arc) => L::data_from_arc(ManuallyDrop::into_inner(arc)),
            ArcOrVTable::Vtable { ptr, vtable } => match unsafe { (vtable.into_arc)(ptr) } {
                Some(arc) => L::data_from_arc(unsafe { Arc::<T>::from_raw(arc) }),
                None => L::data_from_static(unsafe { static_slice(start, length) }),
            },
        }
    }
}
