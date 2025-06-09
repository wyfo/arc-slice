#![no_std]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod buf;
mod bytes;
mod bytes_mut;
#[cfg(feature = "serde")]
mod serde;

pub use crate::{
    buf::{Buf, BufMut},
    bytes::Bytes,
    bytes_mut::BytesMut,
};

#[inline(always)]
#[cfg(feature = "std")]
fn saturating_sub_usize_u64(a: usize, b: u64) -> usize {
    use core::convert::TryFrom;
    match usize::try_from(b) {
        Ok(b) => a.saturating_sub(b),
        Err(_) => 0,
    }
}

#[inline(always)]
#[cfg(feature = "std")]
fn min_u64_usize(a: u64, b: usize) -> usize {
    use core::convert::TryFrom;
    match usize::try_from(a) {
        Ok(a) => usize::min(a, b),
        Err(_) => b,
    }
}

/// Error type for the `try_get_` methods of [`Buf`].
/// Indicates that there were not enough remaining
/// bytes in the buffer while attempting
/// to get a value from a [`Buf`] with one
/// of the `try_get_` methods.
#[derive(Debug, PartialEq, Eq)]
pub struct TryGetError {
    /// The number of bytes necessary to get the value
    pub requested: usize,

    /// The number of bytes available in the buffer
    pub available: usize,
}

impl core::fmt::Display for TryGetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> Result<(), core::fmt::Error> {
        write!(
            f,
            "Not enough bytes remaining in buffer to read value (requested {} but only {} available)",
            self.requested,
            self.available
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for TryGetError {}

#[cfg(feature = "std")]
impl From<TryGetError> for std::io::Error {
    fn from(error: TryGetError) -> Self {
        std::io::Error::new(std::io::ErrorKind::Other, error)
    }
}

/// Panic with a nice error message.
#[cold]
fn panic_advance(error_info: &TryGetError) -> ! {
    panic!(
        "advance out of bounds: the len is {} but advancing by {}",
        error_info.available, error_info.requested
    );
}

#[cold]
fn panic_does_not_fit(size: usize, nbytes: usize) -> ! {
    panic!(
        "size too large: the integer type can fit {} bytes, but nbytes is {}",
        size, nbytes
    );
}

impl<L: arc_slice::layout::Layout> Buf for arc_slice::ArcBytes<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

impl<L: arc_slice::layout::LayoutMut> Buf for arc_slice::ArcBytesMut<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

unsafe impl<L: arc_slice::layout::LayoutMut> BufMut for arc_slice::ArcBytesMut<L> {
    fn remaining_mut(&self) -> usize {
        self.capacity() - self.len()
    }

    unsafe fn advance_mut(&mut self, cnt: usize) {
        // SAFETY: same function contract
        unsafe { self.set_len(self.len() + cnt) }
    }

    fn chunk_mut(&mut self) -> &mut buf::UninitSlice {
        // SAFETY: `UninitSlice` prevent writing uninitialized memory
        unsafe { self.spare_capacity_mut() }.into()
    }
}

impl<L: arc_slice::layout::Layout> Buf for arc_slice::ArcStr<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_bytes()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

impl<
        S: arc_slice::buffer::Slice<Item = u8> + arc_slice::buffer::Subsliceable + ?Sized,
        L: arc_slice::layout::Layout,
    > Buf for arc_slice::__private::SmallSlice<S, L>
{
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        arc_slice::buffer::Slice::to_slice(&**self)
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

impl<
        S: arc_slice::buffer::Slice<Item = u8> + arc_slice::buffer::Subsliceable + ?Sized,
        L: arc_slice::layout::Layout,
    > Buf for arc_slice::__private::SmallArcSlice<S, L>
{
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        arc_slice::buffer::Slice::to_slice(&**self)
    }

    fn advance(&mut self, cnt: usize) {
        self._advance(cnt);
    }
}
