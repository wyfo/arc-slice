//! The different layouts used by [`ArcSlice`] and [`ArcSliceMut`].
//!
//! A layout defines how the data is stored, impacting memory size and the behavior
//! of some operations like `clone`.
//!
//! The default layout for [`ArcSlice`] and [`ArcSliceMut`] can be overridden using
//! [crate features](crate#features).
//!
//! ## Which layout to choose
//!
//! The default layout, [`ArcLayout`], should support most of the use cases efficiently.
//! The other layouts are designed to support some particular buffers without allocating an inner
//! Arc:
//! - [`BoxedSliceLayout`] and [`VecLayout`] are intended for boxed slice/vector buffers,
//!   and should be used only when clones are unlikely;
//! - [`RawLayout`] should be used with [`Arc`] and other raw buffers.
//!
//! Since layout primarily affects [`ArcSlice`]/[`ArcSliceMut`] instantiation, libraries generally
//! don’t need to worry about it: they can either accept the default layout or use a generic one
//! in public APIs, and expose the most appropriate layout in their return types. Libraries should not
//! override the default layout using [crate features], as it would impact every other crates.
//! <br>
//! Binaries should use the default layout, adapting it to the use case using [crate features].
//!
//! In any case, layout choice is primarily a performance concern and should be supported by
//! measurement.
//!
//! ## Layouts summary
//!
//! | Layout             | `ArcSlice` size          | static/empty slices support | arbitrary buffers support | may allocate on clone | optimized for      |
//! |--------------------|--------------------------|-----------------------------|---------------------------|-----------------------|--------------------|
//! | `ArcLayout`        | `3 * size_of::<usize>()` | yes (optional)              | yes (optional)            | no                    | regular `ArcSlice` |
//! | `BoxedSliceLayout` | `3 * size_of::<usize>()` | yes                         | yes                       | yes                   | `Box<[T]>`         |
//! | `VecLayout`        | `4 * size_of::<usize>()` | yes                         | yes                       | yes                   | `Vec<T>`           |
//! | `RawLayout`        | `4 * size_of::<usize>()` | yes                         | yes                       | no                    | `RawBuffer`        |
//!
//! [crate feature]: crate#features
//! [`Arc`]: alloc::sync::Arc

#[cfg(doc)]
use crate::{slice::ArcSlice, slice_mut::ArcSliceMut};

/// A layout, which defines how [`ArcSlice`] data is stored.
pub trait Layout: private::Layout {}
/// A layout, which defines how [`ArcSliceMut`] data is stored.
pub trait LayoutMut: Layout + private::LayoutMut {}

/// A layout that supports arbitrary buffers, such as [`Vec`](alloc::vec::Vec),
/// shared memory regions, ffi buffers, etc.
///
/// It enables [`ArcSlice::from_buffer`]/[`ArcSliceMut::from_buffer`] and derived methods.
pub trait AnyBufferLayout: Layout {}
/// A layout that supports static slices without inner Arc allocation.
///
/// It enables [`ArcSlice::new`] and [`ArcSlice::from_static`]. Additionally, empty subslices are
/// stored as static slices to avoid Arc clone/drop overhead.
pub trait StaticLayout: Layout {}
/// A layout that supports [`clone`](ArcSlice::clone) without allocating.
pub trait CloneNoAllocLayout: Layout {}
/// A layout that supports [`truncate`](ArcSlice::truncate) without allocating.
pub trait TruncateNoAllocLayout: Layout {}

/// The default and most optimized layout.
///
/// It aims to be more performant than other layouts for supported operations,
/// though other layouts may support a broader range of use cases.
/// It takes two generic boolean parameters, whose defaults can be overridden via compilation
/// features:
/// - `ANY_BUFFER`, default to false, if it supports arbitrary buffer;
/// - `STATIC`, default to false, if it supports static slices without allocations; it
///   enables [`Default`] implementation for [`ArcSlice`], as well as const constructors.
///
/// Other layouts support arbitrary buffers and static slices out of the box, but this flexibility
/// comes at a cost. `ArcLayout` focuses instead on providing the most optimized implementation
/// adapted to each use case.
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

/// Enables storing a boxed slice into an [`ArcSlice`] without requiring a second allocation
/// (for the inner Arc), as long as there is a single instance.
///
/// As soon as the [`ArcSlice`] is cloned (or subsliced), then an inner Arc is allocated. As a
/// consequence, when [`oom-handling` feature](crate#features) is not enabled,
/// `ArcSlice<S, BoxedSliceLayout>` doesn't implement [`Clone`].
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

/// Enables storing a vector into an [`ArcSlice`] without requiring a second allocation
/// (for the inner Arc), as long as there is a single instance.
///
/// As soon as the [`ArcSlice`] is cloned (or subsliced), then an inner Arc is allocated. As a
/// consequence, when [`oom-handling` feature](crate#features) is not enabled,
/// `ArcSlice<S, VecLayout>` doesn't implement [`Clone`].
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

/// Enables storing a [`RawBuffer`], without requiring a second allocation (for the inner Arc).
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
/// Layouts that don't implement [`AnyBufferLayout`] cannot straightforwardly be converted from
/// ones that do. However, the actual underlying buffer may be compatible, for example, an
/// `ArcSlice<[u8], VecLayout>` backed by an Arc buffer can in fact be converted  to an
/// `ArcSlice<[u8], ArcLayout<false>>`. Fallible conversions like
/// [`ArcSlice::try_with_layout`]/[`ArcSliceMut::try_freeze`]/etc. can be used to handle this edge
/// case.
pub trait FromLayout<L: Layout>: Layout {}

impl<const STATIC: bool, L: Layout> FromLayout<ArcLayout<false, STATIC>> for L {}
impl<L1: AnyBufferLayout, L2: AnyBufferLayout> FromLayout<L1> for L2 {}

macro_rules! default_layout {
    ($layout:ty) => {
        /// Default layout used by [`ArcSlice`].
        ///
        /// Can be overridden with [crate features](crate#features).
        pub type DefaultLayout = $layout;
    };
}
cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-raw")] {
        default_layout!(RawLayout);
    } else if #[cfg(feature = "default-layout-vec")] {
        default_layout!(VecLayout);
    } else if #[cfg(feature = "default-layout-boxed-slice")] {
        default_layout!(BoxedSliceLayout);
    } else {
        default_layout!(ArcLayout);
    }
}

macro_rules! default_layout_mut {
    ($layout:ty) => {
        /// Default layout used by [`ArcSliceMut`].
        ///
        /// Can be overridden with [crate features](crate#features).
        pub type DefaultLayoutMut = $layout;
    };
}
cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-mut-vec")] {
        default_layout_mut!(VecLayout);
    } else {
        default_layout_mut!(ArcLayout<{ cfg!(feature = "default-layout-mut-any-buffer") }>);
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
