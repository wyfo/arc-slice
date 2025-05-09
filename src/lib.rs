#![no_std]
extern crate alloc;

#[doc(hidden)]
pub mod __private;
mod arc;
mod atomic;
pub mod buffer;
// #[cfg(feature = "bytes")]
// mod bytes;
pub mod error;
#[cfg(feature = "inlined")]
pub mod inlined;
pub mod layout;
mod macros;
mod msrv;
#[cfg(feature = "serde")]
mod serde;
mod slice;
// mod slice_mut;
mod str;
mod utils;

pub use crate::{
    slice::{ArcSlice, ArcSliceBorrow},
    // slice_mut::ArcSliceMut,
    str::{ArcStr, ArcStrBorrow},
};

pub type ArcBytes<L = layout::DefaultLayout> = ArcSlice<u8, L>;
pub type ArcBytesBorrow<'a, L = layout::DefaultLayout> = ArcSliceBorrow<'a, u8, L>;
// pub type ArcBytesMut<L = layout::DefaultLayoutMut, const UNIQUE: bool = true> =
// ArcSliceMut<u8, L, UNIQUE>;

mod slice_mut {
    #[derive(Debug)]
    pub struct ArcSliceMut<T, L = crate::layout::DefaultLayoutMut>(
        core::marker::PhantomData<(T, L)>,
    );
    pub trait ArcSliceMutLayout {}
    impl<T> ArcSliceMutLayout for T {}
}
