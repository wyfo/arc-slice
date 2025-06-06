//! The different layouts used by [`ArcSlice`] and [`ArcSliceMut`].
//!
//! A layout defines how the data is stored, which impacts the memory size, as well as the behavior
//! of some operations like `clone`.
//!
//! Default layout for [`ArcSlice`] and [`ArcSliceMut`] can be overridden using [crate features](crate#features).
//!
//! ## Which layout to choose
//!
//! The default one, [`ArcLayout`], should support most of the use cases efficiently.
//! The goal of other layouts is to support some particular buffers without allocating an inner
//! Arc:
//! - [`BoxedSliceLayout`] and [`VecLayout`] should be only used for the particular use case
//!   of boxed slice/vector buffers, with a low probability of clone;
//! - [`RawLayout`] should be used with [`Arc`] and other raw buffers.
//!
//! As layout mostly impacts [`ArcSlice`]/[`ArcSliceMut`] instantiation, libraries should not
//! really care about it, and either accept the default layout or a generic one in public API;
//! they can expose the best suited layout in return position. Libraries should not override
//! the default layout using [crate features].
//! <br>
//! Binaries should use the default layout, adapting it to the use case using [crate features].
//!
//! In any case, the choice of layout is primarily a matter of performance, so it should be
//! supported by measurements.
//!
//! [crate feature]: crate#features
//! [`Arc`]: alloc::sync::Arc

#[cfg(doc)]
use crate::{slice::ArcSlice, slice_mut::ArcSliceMut};

/// A layout, which defines how [`ArcSlice`] data is stored.
pub trait Layout: private::Layout {}
/// A layout that supports arbitrary buffers.
///
/// It enables [`ArcSlice::from_buffer`]/[`ArcSliceMut::from_buffer`] and derived methods.
pub trait AnyBufferLayout: Layout {}
/// A layout that supports static slices without inner Arc allocation.
///
/// It enables [`ArcSlice::new`] as well as [`ArcSlice::from_static`]. Empty subslices are also
/// converted as static slices to avoid Arc clone/drop.
pub trait StaticLayout: Layout {}
/// A layout that supports [`clone`](ArcSlice::clone) without allocating.
pub trait CloneNoAllocLayout: Layout {}
/// A layout that supports [`truncate`](ArcSlice::truncate) without allocating.
pub trait TruncateNoAllocLayout: Layout {}
/// A layout, which defines how [`ArcSliceMut`] data is stored.
pub trait LayoutMut: Layout + private::LayoutMut {}

/// The default and most optimized layout.
///
/// It should be more performant than other layouts for the same operations (but other layouts
/// may support more use cases). It comes with two generic boolean parameters:
/// - `ANY_BUFFER`, default to false, if it supports arbitrary buffer;
/// - `STATIC`, default to false, if it supports static slices without allocations; it
///   enables [`Default`] implementation for [`ArcSlice`], as well as const constructors.
///
/// Other layouts support out-of-the-box arbitrary buffer, as well as static slices without
/// allocations. However, this support has a cost, which is why this layout proposes the most
/// possible optimized form, to adapt to any use case.
/// ```rust
/// # use core::mem::size_of;
/// # use arc_slice::{layout::ArcLayout, ArcBytes, ArcBytesMut};
/// assert_eq!(size_of::<ArcBytes<ArcLayout>>(), 3 * size_of::<usize>());
/// assert_eq!(size_of::<ArcBytesMut<ArcLayout>>(), 4 * size_of::<usize>());
/// ```
#[derive(Debug)]
pub struct ArcLayout<
    const ANY_BUFFER: bool = { cfg!(feature = "default-layout-any-buffer") },
    const STATIC: bool = { cfg!(feature = "default-layout-static") },
>;
impl<const ANY_BUFFER: bool, const STATIC: bool> Layout for ArcLayout<ANY_BUFFER, STATIC> {}
impl<const STATIC: bool> AnyBufferLayout for ArcLayout<true, STATIC> {}
impl<const ANY_BUFFER: bool> StaticLayout for ArcLayout<ANY_BUFFER, true> {}
impl<const ANY_BUFFER: bool, const STATIC: bool> CloneNoAllocLayout
    for ArcLayout<ANY_BUFFER, STATIC>
{
}
impl<const ANY_BUFFER: bool, const STATIC: bool> TruncateNoAllocLayout
    for ArcLayout<ANY_BUFFER, STATIC>
{
}
impl<const ANY_BUFFER: bool, const STATIC: bool> LayoutMut for ArcLayout<ANY_BUFFER, STATIC> {}

/// Allows storing a boxed slice into an [`ArcSlice`] without requiring a second allocation
/// (for the inner Arc), as long as there is a single instance.
///
/// As soon as the [`ArcSlice`] is cloned (or subsliced), then an inner Arc is allocated. As a
/// consequence, when OOM handling is not enabled, `ArcSlice<S, BoxedSliceLayout>` doesn't
/// implement [`Clone`].
/// <br>
/// When initializing an [`ArcSlice`] with a vector, there will be no Arc allocation if the
/// vector has no spare capacity.
/// ```rust
/// # use core::mem::size_of;
/// # use arc_slice::{layout::BoxedSliceLayout, ArcBytes};
/// assert_eq!(
///     size_of::<ArcBytes<BoxedSliceLayout>>(),
///     3 * size_of::<usize>()
/// );
/// ```
#[derive(Debug)]
pub struct BoxedSliceLayout;
impl Layout for BoxedSliceLayout {}
impl AnyBufferLayout for BoxedSliceLayout {}
impl StaticLayout for BoxedSliceLayout {}

/// Allows storing a vector into an [`ArcSlice`] without requiring a second allocation
/// (for the inner Arc), as long as there is a single instance.
///
/// As soon as the [`ArcSlice`] is cloned (or subsliced), then an inner Arc is allocated. As a
/// consequence, when OOM handling is not enabled, `ArcSlice<S, VecLayout>` doesn't implement
/// [`Clone`].
/// ```rust
/// # use core::mem::size_of;
/// # use arc_slice::{layout::VecLayout, ArcBytes, ArcBytesMut};
/// assert_eq!(size_of::<ArcBytes<VecLayout>>(), 4 * size_of::<usize>());
/// assert_eq!(size_of::<ArcBytesMut<VecLayout>>(), 4 * size_of::<usize>());
/// ```
#[derive(Debug)]
pub struct VecLayout;
impl Layout for VecLayout {}
impl AnyBufferLayout for VecLayout {}
impl StaticLayout for VecLayout {}
impl TruncateNoAllocLayout for VecLayout {}
impl LayoutMut for VecLayout {}

/// Allows storing a [`RawBuffer`], without requiring a second allocation (for the inner Arc).
/// ```rust
/// # use core::mem::size_of;
/// # use arc_slice::{layout::RawLayout, ArcBytes};
/// assert_eq!(size_of::<ArcBytes<RawLayout>>(), 4 * size_of::<usize>());
/// ```
///
/// [`RawBuffer`]: crate::buffer::RawBuffer
#[cfg(feature = "raw-buffer")]
#[derive(Debug)]
pub struct RawLayout;
#[cfg(feature = "raw-buffer")]
impl Layout for RawLayout {}
#[cfg(feature = "raw-buffer")]
impl StaticLayout for RawLayout {}
#[cfg(feature = "raw-buffer")]
impl AnyBufferLayout for RawLayout {}
#[cfg(feature = "raw-buffer")]
impl CloneNoAllocLayout for RawLayout {}
#[cfg(feature = "raw-buffer")]
impl TruncateNoAllocLayout for RawLayout {}

/// A layout that can be converted from another one.
///
/// Only layouts not implementing [`AnyBufferLayout`] cannot be straightforwardly converted
/// from those implementing it. However, the actual underlying buffer may be compatible,
/// for example, an `ArcSlice<[u8], VecLayout>` backed by an Arc buffer can in fact be converted
/// to an `ArcSlice<[u8], ArcLayout<false>>`. Fallible conversions like
/// [`ArcSlice::try_with_layout`]/[`ArcSliceMut::try_freeze`]/etc. can be used to handle this edge
/// case.
pub trait FromLayout<L: Layout>: Layout {}

impl<const STATIC: bool, L: Layout> FromLayout<ArcLayout<false, STATIC>> for L {}
impl<L1: AnyBufferLayout, L2: AnyBufferLayout> FromLayout<L1> for L2 {}

cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-raw")] {
        /// Default layout used by [`ArcSlice`].
        ///
        /// Can be overridden with [crate features](crate#features)
        pub type DefaultLayout = RawLayout;
    } else if #[cfg(feature = "default-layout-vec")] {
        /// Default layout used by [`ArcSlice`].
        ///
        /// Can be overridden with [crate features](crate#features)
        pub type DefaultLayout = VecLayout;
    } else if #[cfg(feature = "default-layout-boxed-slice")] {
        /// Default layout used by [`ArcSlice`].
        ///
        /// Can be overridden with [crate features](crate#features)
        pub type DefaultLayout = BoxedSliceLayout;
    } else {
        /// Default layout used by [`ArcSlice`].
        ///
        /// Can be overridden with [crate features](crate#features)
        pub type DefaultLayout = ArcLayout;
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-mut-vec")] {
        /// Default layout used by[`ArcSliceMut`].
        ///
        /// Can be overridden with [crate features](crate#features)
        pub type DefaultLayoutMut = VecLayout;
    } else {
        /// Default layout used by[`ArcSliceMut`].
        ///
        /// Can be overridden with [crate features](crate#features)
        pub type DefaultLayoutMut = ArcLayout<{ cfg!(feature = "default-layout-mut-any-buffer") }>;
    }
}

#[cfg(not(feature = "inlined"))]
mod private {
    pub use crate::{slice::ArcSliceLayout as Layout, slice_mut::ArcSliceMutLayout as LayoutMut};
}

#[cfg(feature = "inlined")]
mod private {
    use crate::{inlined::InlinedLayout, slice::ArcSliceLayout};

    pub trait Layout: ArcSliceLayout + InlinedLayout {}
    impl<L> Layout for L where L: ArcSliceLayout + InlinedLayout {}

    pub use crate::slice_mut::ArcSliceMutLayout as LayoutMut;
}
