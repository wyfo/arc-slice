pub trait Layout: private::Layout {}

#[derive(Debug)]
pub struct Compact;
impl Layout for Compact {}

#[derive(Debug)]
pub struct Plain;
impl Layout for Plain {}

#[cfg(feature = "inlined")]
mod private {
    use crate::{inlined::InlinedLayout, slice::ArcSliceLayout};

    pub trait Layout: ArcSliceLayout + InlinedLayout {}

    impl<L> Layout for L where L: ArcSliceLayout + InlinedLayout {}
}

#[cfg(not(feature = "inlined"))]
mod private {
    use crate::slice::ArcSliceLayout;

    pub trait Layout: ArcSliceLayout {}

    impl<L> Layout for L where L: ArcSliceLayout {}
}
