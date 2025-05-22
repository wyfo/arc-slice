pub trait Layout: private::Layout {}
pub trait AnyBufferLayout: Layout {}
pub trait StaticLayout: Layout {}
pub trait CloneNoAllocLayout: Layout {}
pub trait TruncateNoAllocLayout: Layout {}
pub trait LayoutMut: Layout + private::LayoutMut {}

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

#[derive(Debug)]
pub struct BoxedSliceLayout;
impl Layout for BoxedSliceLayout {}
impl AnyBufferLayout for BoxedSliceLayout {}
impl StaticLayout for BoxedSliceLayout {}

#[derive(Debug)]
pub struct VecLayout;
impl Layout for VecLayout {}
impl AnyBufferLayout for VecLayout {}
impl StaticLayout for VecLayout {}
impl TruncateNoAllocLayout for VecLayout {}
impl LayoutMut for VecLayout {}

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

pub trait FromLayout<L: Layout>: Layout {}

impl<const STATIC: bool, L: Layout> FromLayout<ArcLayout<false, STATIC>> for L {}
impl<L1: AnyBufferLayout, L2: AnyBufferLayout> FromLayout<L1> for L2 {}

cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-raw")] {
        pub type DefaultLayout = RawLayout;
    } else if #[cfg(feature = "default-layout-vec")] {
        pub type DefaultLayout = VecLayout;
    } else if #[cfg(feature = "default-layout-boxed-slice")] {
        pub type DefaultLayout = BoxedSliceLayout;
    } else {
        pub type DefaultLayout = ArcLayout;
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "default-layout-mut-vec")] {
        pub type DefaultLayoutMut = VecLayout;
    } else {
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
