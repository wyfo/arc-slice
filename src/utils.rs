use core::{
    any::Any,
    fmt,
    mem::MaybeUninit,
    ops::{Bound, RangeBounds},
    ptr::NonNull,
};

use crate::{
    macros::{is, is_not},
    msrv::{NonZero, Zeroable},
};

#[inline(always)]
pub(crate) fn try_transmute<T: Any, U: Any>(any: T) -> Result<U, T> {
    if is_not!(T, U) {
        return Err(any);
    }
    let mut res = MaybeUninit::<U>::uninit();
    unsafe { res.as_mut_ptr().cast::<T>().write(any) };
    Ok(unsafe { res.assume_init() })
}

#[inline(always)]
pub(crate) fn try_transmute_slice<T: Any, U: Any>(slice: &[T]) -> Option<&[U]> {
    is!(T, U).then(|| unsafe { slice.align_to().1 })
}

pub(crate) const fn slice_into_raw_parts<T>(slice: &[T]) -> (NonNull<T>, usize) {
    (
        // MSRV 1.65 const `<*const _>::cast_mut` + 1.85 const `NonNull::new`
        unsafe { NonNull::new_unchecked(slice.as_ptr() as _) },
        slice.len(),
    )
}

pub(crate) unsafe fn static_slice<T: 'static>(start: NonNull<T>, length: usize) -> &'static [T] {
    unsafe { core::slice::from_raw_parts(start.as_ptr(), length) }
}

/// Alternative implementation of `std::fmt::Debug` for byte slice.
///
/// Standard `Debug` implementation for `[u8]` is comma separated
/// list of numbers. Since large amount of byte strings are in fact
/// ASCII strings or contain a lot of ASCII strings (e. g. HTTP),
/// it is convenient to print strings as ASCII when possible.
fn debug_bytes(bytes: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "b\"")?;
    for &b in bytes {
        // https://doc.rust-lang.org/reference/tokens.html#byte-escapes
        if b == b'\n' {
            write!(f, "\\n")?;
        } else if b == b'\r' {
            write!(f, "\\r")?;
        } else if b == b'\t' {
            write!(f, "\\t")?;
        } else if b == b'\\' || b == b'"' {
            write!(f, "\\{}", b as char)?;
        } else if b == b'\0' {
            write!(f, "\\0")?;
        // ASCII printable
        } else if (0x20..0x7f).contains(&b) {
            write!(f, "{}", b as char)?;
        } else {
            write!(f, "\\x{b:02x}")?;
        }
    }
    write!(f, "\"")?;
    Ok(())
}

pub(crate) fn debug_slice<T>(slice: &[T], f: &mut fmt::Formatter<'_>) -> fmt::Result
where
    T: fmt::Debug + 'static,
{
    match try_transmute_slice(slice) {
        Some(bytes) => debug_bytes(bytes, f),
        None => write!(f, "{slice:?}"),
    }
}

pub(crate) fn lower_hex(slice: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for &b in slice {
        write!(f, "{b:02x}")?;
    }
    Ok(())
}

pub(crate) fn upper_hex(slice: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for &b in slice {
        write!(f, "{b:02X}")?;
    }
    Ok(())
}

pub(crate) fn offset_len(len: usize, range: impl RangeBounds<usize>) -> (usize, usize) {
    let offset = match range.start_bound() {
        Bound::Included(&n) => n,
        Bound::Excluded(&n) => n.checked_add(1).unwrap_or_else(|| panic_invalid_range()),
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&n) => n.checked_add(1).unwrap_or_else(|| panic_invalid_range()),
        Bound::Excluded(&n) => n,
        Bound::Unbounded => len,
    };
    if end > len {
        panic_out_of_range();
    }
    let len = end
        .checked_sub(offset)
        .unwrap_or_else(|| panic_invalid_range());
    (offset, len)
}

pub(crate) fn offset_len_subslice<T>(slice: &[T], subslice: &[T]) -> Option<(usize, usize)> {
    let offset = (subslice.as_ptr() as usize).checked_sub(slice.as_ptr() as usize)?;
    let len = subslice.len();
    if offset + len > slice.len() {
        return None;
    }
    Some((offset, len))
}

#[cold]
fn panic_invalid_range() -> ! {
    panic!("invalid range")
}

#[cold]
pub(crate) fn panic_out_of_range() -> ! {
    panic!("out of range")
}

#[cfg(feature = "abort-on-refcount-overflow")]
#[inline(never)]
#[cold]
pub(crate) fn abort() -> ! {
    #[cfg(feature = "std")]
    {
        extern crate std;
        std::process::abort();
    }
    // in no_std, use double panic
    #[cfg(not(feature = "std"))]
    {
        struct Abort;
        impl Drop for Abort {
            fn drop(&mut self) {
                panic!("abort");
            }
        }
        let _guard = Abort;
        panic!("abort");
    }
}

extern "C" {
    #[link_name = "__arc_slice__unreachable_checked__"]
    fn __unreachable_checked() -> !;
}

#[inline(always)]
pub(crate) fn unreachable_checked() -> ! {
    #[cfg(debug_assertions)]
    unreachable!();
    #[cfg(not(debug_assertions))]
    unsafe {
        __unreachable_checked()
    };
}

#[inline(always)]
pub(crate) fn assert_checked(predicate: bool) {
    if !predicate {
        unreachable_checked();
    }
}

pub(crate) trait UnwrapChecked<T> {
    fn unwrap_checked(self) -> T;
}

impl<T> UnwrapChecked<T> for Option<T> {
    #[inline(always)]
    fn unwrap_checked(self) -> T {
        self.unwrap_or_else(|| unreachable_checked())
    }
}

impl<T, E> UnwrapChecked<T> for Result<T, E> {
    #[inline(always)]
    fn unwrap_checked(self) -> T {
        self.unwrap_or_else(|_| unreachable_checked())
    }
}

pub(crate) trait NewChecked<Arg> {
    fn new_checked(arg: Arg) -> Self;
}

impl<T: ?Sized> NewChecked<*mut T> for NonNull<T> {
    #[inline(always)]
    fn new_checked(arg: *mut T) -> Self {
        Self::new(arg).unwrap_checked()
    }
}

impl<T: Zeroable> NewChecked<T> for NonZero<T> {
    #[inline(always)]
    fn new_checked(arg: T) -> Self {
        NonZero::new(arg).unwrap_checked()
    }
}

#[inline(always)]
pub(crate) fn transmute_checked<T: Any, U: Any>(any: T) -> U {
    try_transmute(any).unwrap_checked()
}

// from `Vec` implementation
pub(crate) const fn min_non_zero_cap<T>() -> usize {
    if core::mem::size_of::<T>() == 1 {
        8
    } else if core::mem::size_of::<T>() <= 1024 {
        4
    } else {
        1
    }
}
