//! A utility library for working with shared slices of memory.
//!
//! The crate provides efficient shared buffer implementations [`ArcSlice`]/[`ArcSliceMut`].
//!
//! ```rust
//! use arc_slice::{ArcSlice, ArcSliceMut};
//!
//! let mut bytes_mut: ArcSliceMut<[u8]> = ArcSliceMut::new();
//! bytes_mut.extend_from_slice(b"Hello world");
//!
//! let mut bytes: ArcSlice<[u8]> = bytes_mut.freeze();
//!
//! let a: ArcSlice<[u8]> = bytes.subslice(0..5);
//! assert_eq!(a, b"Hello");
//!
//! let b: ArcSlice<[u8]> = bytes.split_to(6);
//! assert_eq!(bytes, b"world");
//! assert_eq!(b, b"Hello ");
//! ```
//!
//! Depending on its [layout], [`ArcSlice`] can also support arbitrary buffer, e.g. shared memory,
//! and provides optional metadata that can be attached to the buffer.
//!
//! ```rust
//! use std::{
//!     fs::File,
//!     path::{Path, PathBuf},
//! };
//!
//! use arc_slice::{buffer::AsRefBuffer, layout::ArcLayout, ArcBytes};
//! use memmap2::Mmap;
//!
//! # fn main() -> std::io::Result<()> {
//! let path = Path::new("README.md").to_owned();
//! # #[cfg(not(miri))]
//! let file = File::open(&path)?;
//! # #[cfg(not(miri))]
//! let mmap = unsafe { Mmap::map(&file)? };
//! # #[cfg(miri)]
//! # let mmap = b"# arc-slice".to_vec();
//!
//! let buffer = AsRefBuffer(mmap);
//! let bytes: ArcBytes<ArcLayout<true>> = ArcBytes::from_buffer_with_metadata(buffer, path);
//! assert!(bytes.starts_with(b"# arc-slice"));
//! assert_eq!(bytes.metadata::<PathBuf>().unwrap(), Path::new("README.md"));
//! # Ok(())
//! # }
//! ```
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
//! - `inlined`: enable [Small String Optimization] for [`ArcSlice`] with
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
//! [Small String Optimization]: https://cppdepend.com/blog/understanding-small-string-optimization-sso-in-stdstring/
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

/// An alias for `ArcSlice<[u8], L>`.
pub type ArcBytes<L = layout::DefaultLayout> = ArcSlice<[u8], L>;
/// An alias for `ArcSliceBorrow<[u8], L>`.
pub type ArcBytesBorrow<'a, L = layout::DefaultLayout> = ArcSliceBorrow<'a, [u8], L>;
/// An alias for `ArcSliceMut<[u8], L>`.
pub type ArcBytesMut<L = layout::DefaultLayoutMut, const UNIQUE: bool = true> =
    ArcSliceMut<[u8], L, UNIQUE>;
/// An alias for `ArcSlice<str, L>`.
pub type ArcStr<L = layout::DefaultLayout> = ArcSlice<str, L>;
/// An alias for `ArcSliceBorrow<str, L>`.
pub type ArcStrBorrow<'a, L = layout::DefaultLayout> = ArcSliceBorrow<'a, str, L>;
/// An alias for `ArcSliceMut<str, L>`.
pub type ArcStrMut<L = layout::DefaultLayoutMut, const UNIQUE: bool = true> =
    ArcSliceMut<str, L, UNIQUE>;
