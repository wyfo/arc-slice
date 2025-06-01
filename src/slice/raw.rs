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
    buffer::{Buffer, DynBuffer, RawBuffer, Slice, SliceExt},
    error::AllocErrorImpl,
    layout::RawLayout,
    msrv::ptr,
    slice::ArcSliceLayout,
    slice_mut,
    slice_mut::ArcSliceMutLayout,
    vtable::{generic_take_buffer, VTable},
};

mod static_vtable {
    use core::{any::TypeId, ptr::NonNull};

    #[allow(unused_imports)]
    use crate::msrv::NonNullExt;
    use crate::{
        buffer::{Slice, SliceExt},
        error::AllocError,
        macros::is_not,
        vtable::{no_capacity, VTable},
    };

    unsafe fn deallocate(_ptr: *mut ()) {}
    unsafe fn is_buffer_unique(_ptr: *const ()) -> bool {
        false
    }
    unsafe fn get_metadata(_ptr: *const (), _type_id: TypeId) -> Option<NonNull<()>> {
        None
    }
    unsafe fn take_buffer<S: Slice + ?Sized>(
        buffer: NonNull<()>,
        _ptr: *const (),
        type_id: TypeId,
        start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>> {
        if is_not!({ type_id }, &'static S) {
            return None;
        }
        unsafe { buffer.cast().write(S::from_raw_parts(start.cast(), length)) };
        Some(buffer)
    }
    unsafe fn drop(_ptr: *const ()) {}
    unsafe fn drop_with_unique_hint(_ptr: *const ()) {}
    unsafe fn clone(_ptr: *const ()) {}
    unsafe fn into_arc(_ptr: *const ()) -> Option<NonNull<()>> {
        None
    }
    unsafe fn into_arc_fallible(_ptr: *const ()) -> Result<Option<NonNull<()>>, AllocError> {
        Ok(None)
    }

    pub(super) const fn new_vtable<S: Slice + ?Sized>() -> &'static VTable {
        &VTable {
            deallocate,
            drop,
            drop_with_unique_hint,
            clone,
            is_buffer_unique,
            get_metadata,
            take_buffer: take_buffer::<S>,
            capacity: no_capacity,
            try_reserve: None,
            into_arc,
            into_arc_fallible,
        }
    }
}

mod raw_vtable {
    use core::{any::TypeId, convert::Infallible, mem, mem::ManuallyDrop, ptr::NonNull};

    #[allow(unused_imports)]
    use crate::msrv::NonNullExt;
    use crate::{
        arc::Arc,
        buffer::{DynBuffer, RawBuffer, Slice, SliceExt},
        error::{AllocError, AllocErrorImpl},
        macros::{is, is_not},
        utils::UnwrapChecked,
        vtable::{no_capacity, VTable},
    };

    unsafe fn deallocate(_ptr: *mut ()) {
        unreachable!()
    }

    unsafe fn is_buffer_unique<S: ?Sized, B: RawBuffer<S>>(ptr: *const ()) -> bool {
        ManuallyDrop::new(unsafe { B::from_raw(ptr) }).is_unique()
    }

    unsafe fn get_metadata<S: ?Sized, B: DynBuffer + RawBuffer<S>>(
        ptr: *const (),
        type_id: TypeId,
    ) -> Option<NonNull<()>> {
        if is!(B::Metadata, ()) || is_not!({ type_id }, B::Metadata) {
            return None;
        }
        Some(NonNull::from(ManuallyDrop::new(unsafe { B::from_raw(ptr) }).get_metadata()).cast())
    }

    unsafe fn take_buffer<S: Slice + ?Sized, B: DynBuffer + RawBuffer<S>>(
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

    unsafe fn drop<S: ?Sized, B: RawBuffer<S>>(ptr: *const ()) {
        mem::drop(unsafe { B::from_raw(ptr) });
    }
    unsafe fn clone<S: ?Sized, B: RawBuffer<S>>(ptr: *const ()) {
        let _ = (*ManuallyDrop::new(unsafe { B::from_raw(ptr) })).clone();
    }

    unsafe fn into_arc<S: Slice + ?Sized, B: DynBuffer + RawBuffer<S>>(
        ptr: *const (),
    ) -> Option<NonNull<()>> {
        let buffer = unsafe { B::from_raw(ptr) };
        let (arc, _, _) = Arc::<S>::new_buffer::<_, Infallible>(buffer).unwrap_checked();
        Some(arc.into_raw())
    }

    unsafe fn into_arc_fallible<S: Slice + ?Sized, B: DynBuffer + RawBuffer<S>>(
        ptr: *const (),
    ) -> Result<Option<NonNull<()>>, AllocError> {
        let buffer = unsafe { B::from_raw(ptr) };
        let (arc, _, _) = Arc::<S>::new_buffer::<_, AllocError>(buffer)
            .map_err(|(err, buffer)| err.forget(buffer))?;
        Ok(Some(arc.into_raw()))
    }

    pub(super) const fn new_vtable<S: Slice + ?Sized, B: DynBuffer + RawBuffer<S>>(
    ) -> &'static VTable {
        &VTable {
            deallocate,
            drop: drop::<S, B>,
            drop_with_unique_hint: drop::<S, B>,
            clone: clone::<S, B>,
            is_buffer_unique: is_buffer_unique::<S, B>,
            get_metadata: get_metadata::<S, B>,
            take_buffer: take_buffer::<S, B>,
            capacity: no_capacity,
            try_reserve: None,
            into_arc: into_arc::<S, B>,
            into_arc_fallible: into_arc_fallible::<S, B>,
        }
    }
}

enum ArcOrVTable<S: Slice + ?Sized> {
    Arc(ManuallyDrop<Arc<S, false>>),
    Vtable {
        ptr: *const (),
        vtable: &'static VTable,
    },
}

fn arc_or_vtable<S: Slice + ?Sized>(
    (ptr, vtable): <RawLayout as ArcSliceLayout>::Data,
) -> ArcOrVTable<S> {
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
        Some((ptr::null(), Some(static_vtable::new_vtable::<[()]>())));
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> =
        MaybeUninit::new((ptr::null(), Some(static_vtable::new_vtable::<[()]>())));

    fn data_from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(
        arc: Arc<S, ANY_BUFFER>,
    ) -> Self::Data {
        let vtable = arc.vtable();
        (arc.into_raw().as_ptr(), vtable)
    }

    fn data_from_arc_slice<S: Slice + ?Sized>(arc: Arc<S, false>) -> Self::Data {
        (arc.into_raw().as_ptr(), None)
    }

    fn data_from_arc_buffer<S: Slice + ?Sized, const ANY_BUFFER: bool, B: DynBuffer + Buffer<S>>(
        arc: Arc<S, ANY_BUFFER>,
    ) -> Self::Data {
        (arc.into_raw().as_ptr(), Some(arc_vtable::new::<S, B>()))
    }

    fn data_from_static<S: Slice + ?Sized, E: AllocErrorImpl>(
        _slice: &'static S,
    ) -> Result<Self::Data, (E, &'static S)> {
        Ok((ptr::null(), Some(static_vtable::new_vtable::<S>())))
    }

    fn data_from_vec<S: Slice + ?Sized, E: AllocErrorImpl>(
        vec: S::Vec,
    ) -> Result<Self::Data, (E, S::Vec)> {
        Ok((
            Arc::<S>::new_vec::<E>(vec)?.into_raw().as_ptr(),
            Some(arc_vtable::new_vec::<S>()),
        ))
    }

    fn data_from_raw_buffer<S: Slice + ?Sized, B: DynBuffer + RawBuffer<S>>(
        buffer: *const (),
    ) -> Option<Self::Data> {
        Some((buffer, Some(raw_vtable::new_vtable::<S, B>())))
    }

    fn clone<S: Slice + ?Sized, E: AllocErrorImpl>(
        _start: NonNull<S::Item>,
        _length: usize,
        data: &Self::Data,
    ) -> Result<Self::Data, E> {
        match arc_or_vtable::<S>(*data) {
            ArcOrVTable::Arc(arc) => mem::forget((*arc).clone()),
            ArcOrVTable::Vtable { ptr, vtable } => unsafe { (vtable.clone)(ptr) },
        }
        Ok(*data)
    }

    unsafe fn drop<S: Slice + ?Sized, const UNIQUE_HINT: bool>(
        _start: NonNull<S::Item>,
        _length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) {
        match arc_or_vtable::<S>(**data) {
            ArcOrVTable::Arc(arc) => {
                ManuallyDrop::into_inner(arc).drop_with_unique_hint::<UNIQUE_HINT>();
            }
            ArcOrVTable::Vtable { ptr, vtable } if UNIQUE_HINT => unsafe {
                (vtable.drop_with_unique_hint)(ptr);
            },
            ArcOrVTable::Vtable { ptr, vtable } => unsafe { (vtable.drop)(ptr) },
        }
    }

    fn is_unique<S: Slice + ?Sized>(data: &Self::Data) -> bool {
        match arc_or_vtable::<S>(*data) {
            ArcOrVTable::Arc(arc) => arc.is_buffer_unique(),
            ArcOrVTable::Vtable { ptr, vtable } => unsafe { (vtable.is_buffer_unique)(ptr) },
        }
    }

    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Self::Data) -> Option<&M> {
        match arc_or_vtable::<S>(*data) {
            ArcOrVTable::Arc(arc) => Some(unsafe { &*ptr::from_ref(arc.get_metadata::<M>()?) }),
            ArcOrVTable::Vtable { ptr, vtable } => unsafe {
                let metadata = (vtable.get_metadata)(ptr, TypeId::of::<M>())?;
                Some(metadata.cast().as_ref())
            },
        }
    }

    unsafe fn take_buffer<S: Slice + ?Sized, B: Buffer<S>>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B> {
        match arc_or_vtable::<S>(**data) {
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

    unsafe fn take_array<T: Send + Sync + 'static, const N: usize>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<[T; N]> {
        match arc_or_vtable::<[T]>(**data) {
            ArcOrVTable::Arc(arc) => {
                unsafe { ManuallyDrop::into_inner(arc).take_array::<N, false>(start, length) }
                    .map_err(mem::forget)
                    .ok()
            }
            _ => None,
        }
    }

    unsafe fn mut_data<S: Slice + ?Sized, L: ArcSliceMutLayout>(
        start: NonNull<S::Item>,
        _length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<slice_mut::Data>)> {
        match arc_or_vtable::<S>(**data) {
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

    fn update_layout<S: Slice + ?Sized, L: ArcSliceLayout, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        data: Self::Data,
    ) -> Option<L::Data> {
        let res = match arc_or_vtable::<S>(data) {
            ArcOrVTable::Arc(arc) => return L::try_data_from_arc(arc),
            _ if !L::ANY_BUFFER => return None,
            ArcOrVTable::Vtable { ptr, vtable } if E::FALLIBLE => unsafe {
                (vtable.into_arc_fallible)(ptr)
            },
            ArcOrVTable::Vtable { ptr, vtable } => Ok(unsafe { (vtable.into_arc)(ptr) }),
        };
        match res {
            Ok(Some(arc)) => Some(L::data_from_arc(unsafe { Arc::<S>::from_raw(arc) })),
            Ok(None) => {
                L::data_from_static::<_, E>(unsafe { S::from_raw_parts(start, length) }).ok()
            }
            Err(_) => None,
        }
    }
}
