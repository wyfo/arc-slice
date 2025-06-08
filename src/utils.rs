use core::{
    any::Any,
    convert::Infallible,
    fmt,
    mem::MaybeUninit,
    ops::{Bound, RangeBounds},
    ptr::NonNull,
};

#[allow(unused_imports)]
use crate::msrv::StrictProvenance;
use crate::{
    buffer::{Slice, SliceExt, Subsliceable},
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
pub(crate) fn try_as_bytes<S: Slice + ?Sized>(slice: &S) -> Option<&[u8]> {
    is!(&'static S, &'static [u8]).then(|| unsafe { slice.to_slice().align_to().1 })
}

pub(crate) fn debug_slice<S: fmt::Debug + Slice + ?Sized>(
    slice: &S,
    f: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    match try_as_bytes(slice) {
        Some(bytes) => write!(f, "b\"{}\"", bytes.escape_ascii()),
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

pub(crate) fn range_offset_len<S: Subsliceable + ?Sized>(
    slice: &S,
    range: impl RangeBounds<usize>,
) -> (usize, usize) {
    let offset = match range.start_bound() {
        Bound::Included(&n) => n,
        Bound::Excluded(&n) => n.checked_add(1).unwrap_or_else(|| panic_invalid_range()),
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&n) => n.checked_add(1).unwrap_or_else(|| panic_invalid_range()),
        Bound::Excluded(&n) => n,
        Bound::Unbounded => slice.len(),
    };
    if end > slice.len() {
        panic_out_of_range();
    }
    let len = end
        .checked_sub(offset)
        .unwrap_or_else(|| panic_invalid_range());
    unsafe { slice.check_subslice(offset, end) };
    (offset, len)
}

pub(crate) fn subslice_offset_len<S: Subsliceable + ?Sized>(
    slice: &S,
    subslice: &S,
) -> (usize, usize) {
    let sub_start = subslice.as_ptr().addr().get();
    let start = slice.as_ptr().addr().get();
    let offset = sub_start
        .checked_sub(start)
        .unwrap_or_else(|| panic_out_of_range());
    if offset + subslice.len() > slice.len() {
        panic_out_of_range()
    }
    unsafe { slice.check_subslice(offset, offset + subslice.len()) };
    (offset, subslice.len())
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

pub(crate) trait UnwrapInfallible<T> {
    fn unwrap_infallible(self) -> T;
}

impl<T> UnwrapInfallible<T> for Result<T, Infallible> {
    fn unwrap_infallible(self) -> T {
        match self {
            Ok(v) => v,
            Err(infallible) => match infallible {},
        }
    }
}

impl<T, U> UnwrapInfallible<T> for Result<T, (Infallible, U)> {
    fn unwrap_infallible(self) -> T {
        match self {
            Ok(v) => v,
            Err((infallible, _)) => match infallible {},
        }
    }
}

extern "C" {
    #[link_name = "__arc_slice__unreachable_checked__"]
    fn __unreachable_checked() -> !;
}

#[inline(always)]
pub(crate) fn unreachable_checked() -> ! {
    #[cfg(not(unreachable_checked))]
    unreachable!();
    #[cfg(unreachable_checked)]
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
