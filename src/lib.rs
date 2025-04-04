#![no_std]
extern crate alloc;

#[doc(hidden)]
pub mod __private;
mod arc;
pub mod buffer;
#[cfg(feature = "bytes")]
mod bytes;
pub mod error;
#[cfg(feature = "inlined")]
pub mod inlined;
pub mod layout;
mod loom;
mod macros;
mod msrv;
#[cfg(feature = "serde")]
mod serde;
mod slice;
mod slice_mut;
mod str;
mod utils;

pub use crate::{
    slice::{ArcSlice, ArcSliceRef},
    slice_mut::ArcSliceMut,
    str::{ArcStr, ArcStrRef},
};

pub type ArcBytes<L = layout::Compact> = ArcSlice<u8, L>;
pub type ArcBytesRef<'a, L = layout::Compact> = ArcSliceRef<'a, u8, L>;
pub type ArcBytesMut = ArcSliceMut<u8>;
