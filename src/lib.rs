#![no_std]
extern crate alloc;

#[doc(hidden)]
pub mod __private;
mod arc;
mod atomic;
#[cfg(feature = "bstr")]
mod bstr;
pub mod buffer;
#[cfg(feature = "bytes")]
mod bytes;
pub mod error;
#[cfg(feature = "inlined")]
pub mod inlined;
pub mod layout;
mod macros;
mod msrv;
#[cfg(feature = "serde")]
mod serde;
mod slice;
mod slice_mut;
mod utils;
mod vtable;

pub use crate::{
    slice::{ArcSlice, ArcSliceBorrow},
    slice_mut::ArcSliceMut,
};

pub type ArcBytes<L = layout::DefaultLayout> = ArcSlice<[u8], L>;
pub type ArcBytesBorrow<'a, L = layout::DefaultLayout> = ArcSliceBorrow<'a, [u8], L>;
pub type ArcBytesMut<L = layout::DefaultLayoutMut, const UNIQUE: bool = true> =
    ArcSliceMut<[u8], L, UNIQUE>;
pub type ArcStr<L = layout::DefaultLayout> = ArcSlice<str, L>;
pub type ArcStrBorrow<'a, L = layout::DefaultLayout> = ArcSliceBorrow<'a, str, L>;
pub type ArcStrMut<L = layout::DefaultLayoutMut, const UNIQUE: bool = true> =
    ArcSliceMut<str, L, UNIQUE>;
