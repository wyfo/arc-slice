pub trait Layout: private::Layout {}
pub trait AnyBufferLayout: Layout {}
pub trait StaticLayout: Layout {}
pub trait LayoutMut: Layout + private::LayoutMut {}

#[derive(Debug)]
pub struct SimpleLayout<
    const ANY_BUFFER: bool = { cfg!(feature = "default-layout-any-buffer") },
    const STATIC: bool = { cfg!(feature = "default-layout-static") },
>;
impl<const ANY_BUFFER: bool, const STATIC: bool> Layout for SimpleLayout<ANY_BUFFER, STATIC> {}
impl<const STATIC: bool> AnyBufferLayout for SimpleLayout<true, STATIC> {}
impl<const ANY_BUFFER: bool> StaticLayout for SimpleLayout<ANY_BUFFER, true> {}

#[derive(Debug)]
pub struct BoxedSliceLayout;
impl Layout for BoxedSliceLayout {}
impl StaticLayout for BoxedSliceLayout {}
impl AnyBufferLayout for BoxedSliceLayout {}

#[derive(Debug)]
pub struct VecLayout;
impl Layout for VecLayout {}
impl StaticLayout for VecLayout {}
impl AnyBufferLayout for VecLayout {}

#[derive(Debug)]
pub struct RawLayout<const BOXED_SLICE: bool = { cfg!(feature = "default-layout-boxed-slice") }>;
// impl<const BOXED_SLICE: bool> Layout for RawLayout<BOXED_SLICE> {}
// impl<const BOXED_SLICE: bool> StaticLayout for RawLayout<BOXED_SLICE> {}
// impl<const BOXED_SLICE: bool> AnyBufferLayout for RawLayout<BOXED_SLICE> {}

pub trait FromLayout<L: Layout>: Layout {}

impl<const STATIC: bool, L: Layout> FromLayout<SimpleLayout<false, STATIC>> for L {}
impl<L1: AnyBufferLayout, L2: AnyBufferLayout> FromLayout<L1> for L2 {}

cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-raw")] {
        pub type DefaultLayout = RawLayout;
    } else if #[cfg(feature = "default-layout-vec")] {
        pub type DefaultLayout = VecLayout;
    } else if #[cfg(feature = "default-layout-boxed-slice")] {
        pub type DefaultLayout = BoxedSliceLayout;
    } else {
        pub type DefaultLayout = SimpleLayout;
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-mut-vec")] {
        pub type DefaultLayoutMut = VecLayout;
    } else {
        pub type DefaultLayoutMut = SimpleLayout<
            { cfg!(feature = "default-layout-mut-any-buffer") },
            { cfg!(feature = "default-layout-static") },
        >;
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
