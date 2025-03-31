#[cfg(not(feature = "inlined"))]
mod not_inlined {
    macro_rules! dummy {
        ($($name:ident),*) => {$(
            #[derive(Debug)]
            pub struct $name<L = ()>(core::marker::PhantomData<L>);
            impl<L> $name<L> {
                pub fn len(&self) -> usize {
                    unimplemented!()
                }
                pub fn is_empty(&self) -> bool {
                    unimplemented!()
                }
                pub fn capacity(&self) -> usize {
                    unimplemented!()
                }
                pub fn as_slice(&self) -> &[u8] {
                    unimplemented!()
                }
                pub fn advance(&mut self, _cnt: usize) {
                    unimplemented!()
                }
                pub fn _advance(&mut self, _cnt: usize) {
                    unimplemented!()
                }
                pub unsafe fn set_len(&mut self, _len: usize) {
                    unimplemented!()
                }
                pub unsafe fn spare_capacity_mut(&mut self) -> &mut [core::mem::MaybeUninit<u8>] {
                    unimplemented!()
                }
            }
        )*};
    }
    dummy!(SmallBytes, SmallArcBytes, SmallStr, SmallArcStr);
}
#[cfg(not(feature = "inlined"))]
pub use self::not_inlined::*;
#[cfg(feature = "inlined")]
pub use crate::inlined::*;
