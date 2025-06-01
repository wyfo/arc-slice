//! TODO
//!
//! ## Features
//!
//! The crate defines the following features:
//! - `abort-on-refcount-overflow` (default): abort on refcount overflow; when not enabled,
//!   the refcount is saturated on overflow, leaking the allocated memory, as it is done in
//!   Linux reference counting implementation.
//! - `bstr`: implement slice traits for [`bstr`](::bstr) crate, allowing to use `ArcSlice<BStr>`.
//! - `bytemuck`: use [`bytemuck::Zeroable`] as bound for arbitrary slice initialization with [`ArcSliceMut::zeroed`].
//! - `bytes`: implement [`Buf`](::bytes::Buf)/[`BufMut`](::bytes::BufMut) for [`ArcSlice`]/[`ArcSliceMut`].
//! - `inlined`: enable [small string optimization] for [`ArcSlice`] with
//!   [`inlined::SmallArcSlice`];
//! - `oom-handling` (default): enable global [Out Of Memory handling] and provide infallible
//!   methods involving allocations.
//! - `portable-atomic`: use [`portable_atomic`] instead of [`core::sync::atomic`].
//! - `portable-atomic-util`: implement traits for [`portable_atomic_util::Arc`] instead of
//!   [`alloc::sync::Arc`].
//! - `raw-buffer`: enable [`RawBuffer`](buffer::RawBuffer) and [`RawLayout`](layout::RawLayout).
//! - `serde`: implement [`Serialize`](::serde::Serialize)/[`Deserialize`](::serde::Deserialize)
//!   for [`ArcSlice`]/[`ArcSliceMut`].
//! - `std`: implement various `std` traits, link to the `std` crate.
//!
//! Moreover, it is possible to override default [layout] using the following features:
//! - `default-layout-any-buffer`: override the default value of [`ArcLayout`] `ANY_BUFFER`
//!   to `true`.
//! - `default-layout-static`: override the default value of [`ArcLayout`] `STATIC` to `true`.
//! - `default-layout-boxed-slice`: override the default layout to
//!   [`BoxedSliceLayout`](layout::BoxedSliceLayout).
//! - `default-layout-vec`: override the default layout to [`VecLayout`](layout::VecLayout).
//! - `default-layout-raw`: override the default layout to [`RawLayout`](layout::RawLayout).
//! - `default-layout-any-buffer`: override the default value of [`ArcLayout`] `ANY_BUFFER` to
//!   `true` for [`ArcSliceMut`].
//! - `default-layout-vec`: override the default layout to [`VecLayout`](layout::VecLayout)
//!   for [`ArcSliceMut`].
//!
//! [small string optimization]: https://cppdepend.com/blog/understanding-small-string-optimization-sso-in-stdstring/
//! [Out Of Memory handling]: alloc::alloc::handle_alloc_error
//! [`ArcLayout`]: layout::ArcLayout
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
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
