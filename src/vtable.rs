use core::{
    any::{Any, TypeId},
    mem::MaybeUninit,
    ptr::NonNull,
};

#[allow(unused_imports)]
use crate::msrv::NonNullExt;
use crate::{slice_mut::TryReserveResult, utils::NewChecked};

#[allow(clippy::type_complexity)]
#[derive(Debug)]
pub struct VTable {
    pub(crate) deallocate: unsafe fn(ptr: *mut ()),
    pub(crate) is_buffer_unique: unsafe fn(ptr: *const ()) -> bool,
    pub(crate) get_metadata: unsafe fn(ptr: *const (), type_id: TypeId) -> Option<NonNull<()>>,
    pub(crate) take_buffer: unsafe fn(
        buffer: NonNull<()>,
        ptr: *const (),
        type_id: TypeId,
        start: NonNull<()>,
        length: usize,
    ) -> Option<NonNull<()>>,
    // capacity -> usize::MAX means either not unique or not mutable
    pub(crate) capacity: unsafe fn(ptr: *const (), start: NonNull<()>) -> usize,
    pub(crate) try_reserve: Option<
        unsafe fn(
            ptr: NonNull<()>,
            start: NonNull<()>,
            length: usize,
            additional: usize,
            allocate: bool,
        ) -> TryReserveResult<()>,
    >,
    #[cfg(feature = "raw-buffer")]
    pub(crate) drop: unsafe fn(ptr: *const ()),
    #[cfg(feature = "raw-buffer")]
    pub(crate) drop_with_unique_hint: unsafe fn(ptr: *const ()),
    #[cfg(feature = "raw-buffer")]
    pub(crate) clone: unsafe fn(ptr: *const ()),
    #[cfg(feature = "raw-buffer")]
    pub(crate) into_arc: unsafe fn(ptr: *const ()) -> Option<NonNull<()>>,
}

pub(crate) unsafe fn no_capacity(_ptr: *const (), _start: NonNull<()>) -> usize {
    usize::MAX
}

pub(crate) unsafe fn generic_take_buffer<B: Any>(
    ptr: *const (),
    vtable: &'static VTable,
    start: NonNull<()>,
    length: usize,
) -> Option<B> {
    let mut buffer = MaybeUninit::<B>::uninit();
    let buffer_ptr = NonNull::new_checked(buffer.as_mut_ptr()).cast();
    let type_id = TypeId::of::<B>();
    let buffer_ptr =
        unsafe { (vtable.take_buffer)(buffer_ptr, ptr, type_id, start.cast(), length)? };
    unsafe { Some(buffer_ptr.cast().read()) }
}
