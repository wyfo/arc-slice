use core::{
    fmt,
    ops::{Bound, RangeBounds},
};

use crate::macros::is;

pub(crate) fn transmute_slice<T: 'static, U: 'static>(slice: &[T]) -> Option<&[U]> {
    is!(T, U).then(|| unsafe { slice.align_to().1 })
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
            write!(f, "\\x{:02x}", b)?;
        }
    }
    write!(f, "\"")?;
    Ok(())
}

pub(crate) fn debug_slice<T>(slice: &[T], f: &mut fmt::Formatter<'_>) -> fmt::Result
where
    T: fmt::Debug + 'static,
{
    match transmute_slice(slice) {
        Some(bytes) => debug_bytes(bytes, f),
        None => write!(f, "{slice:?}"),
    }
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

fn offset_len_subslice_impl<T>(slice: &[T], subslice: &[T]) -> Option<(usize, usize)> {
    let offset = (subslice.as_ptr() as usize).checked_sub(slice.as_ptr() as usize)?;
    let len = subslice.len();
    if offset + len > slice.len() {
        return None;
    }
    Some((offset, len))
}

pub(crate) fn offset_len_subslice<T>(slice: &[T], subslice: &[T]) -> (usize, usize) {
    offset_len_subslice_impl(slice, subslice).unwrap_or_else(|| panic_out_of_range())
}

pub(crate) unsafe fn offset_len_subslice_unchecked<T>(
    slice: &[T],
    subslice: &[T],
) -> (usize, usize) {
    unsafe { offset_len_subslice_impl(slice, subslice).unwrap_unchecked() }
}

#[cold]
fn panic_invalid_range() -> ! {
    panic!("invalid range")
}

#[cold]
pub(crate) fn panic_out_of_range() -> ! {
    panic!("out of range")
}
