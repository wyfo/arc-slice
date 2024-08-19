use alloc::boxed::Box;
use core::{num::NonZeroUsize, ptr, ptr::NonNull};

pub(crate) fn without_provenance_mut<T>(addr: usize) -> *mut T {
    ptr::null_mut::<u8>().wrapping_add(addr).cast()
}

pub(crate) fn ptr_addr<T>(ptr: *const T) -> usize {
    ptr as usize
}

pub(crate) fn non_null_addr<T>(ptr: NonNull<T>) -> NonZeroUsize {
    NonZeroUsize::new(ptr.as_ptr() as usize).unwrap()
}

pub(crate) fn non_null_with_addr<T>(ptr: NonNull<T>, addr: NonZeroUsize) -> NonNull<T> {
    let ptr_addr = ptr.as_ptr() as isize;
    let dest_addr = addr.get() as isize;
    let offset = dest_addr.wrapping_sub(ptr_addr);
    unsafe { NonNull::new_unchecked(ptr.as_ptr().cast::<u8>().wrapping_offset(offset).cast()) }
}

pub(crate) fn non_null_map_addr<T>(
    ptr: NonNull<T>,
    f: impl FnOnce(NonZeroUsize) -> NonZeroUsize,
) -> NonNull<T> {
    non_null_with_addr(ptr, f(non_null_addr(ptr)))
}

#[cfg(feature = "inlined")]
pub(crate) const fn ptr_from_ref<T: ?Sized>(ptr: *const T) -> *const T {
    ptr
}

pub(crate) const fn ptr_from_mut<T: ?Sized>(ptr: *mut T) -> *mut T {
    ptr
}

pub(crate) const unsafe fn non_null_add<T>(ptr: NonNull<T>, count: usize) -> NonNull<T> {
    unsafe { NonNull::new_unchecked(ptr.as_ptr().add(count)) }
}

pub(crate) const unsafe fn non_null_write<T>(ptr: NonNull<T>, val: T) {
    unsafe { ptr.as_ptr().write(val) }
}

pub(crate) unsafe fn sub_ptr<T>(ptr: *const T, origin: *const T) -> usize {
    unsafe { ptr.offset_from(origin).try_into().unwrap_unchecked() }
}

pub(crate) fn box_into_nonnull<T>(ptr: Box<T>) -> NonNull<T> {
    NonNull::new(Box::into_raw(ptr)).unwrap()
}
