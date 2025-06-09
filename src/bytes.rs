use crate::{
    buffer::{Extendable, Slice, Subsliceable},
    layout::{Layout, LayoutMut},
    ArcSlice, ArcSliceMut,
};

impl<S: Slice<Item = u8> + Subsliceable + ?Sized, L: Layout> bytes::Buf for ArcSlice<S, L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.to_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

impl<S: Slice<Item = u8> + Subsliceable + ?Sized, L: LayoutMut, const UNIQUE: bool> bytes::Buf
    for ArcSliceMut<S, L, UNIQUE>
{
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.to_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

unsafe impl<S: Slice<Item = u8> + Extendable + ?Sized, L: LayoutMut, const UNIQUE: bool>
    bytes::BufMut for ArcSliceMut<S, L, UNIQUE>
{
    fn remaining_mut(&self) -> usize {
        self.capacity() - self.len()
    }

    unsafe fn advance_mut(&mut self, cnt: usize) {
        // SAFETY: same function contract
        unsafe { self.set_len(self.len() + cnt) }
    }

    fn chunk_mut(&mut self) -> &mut bytes::buf::UninitSlice {
        // SAFETY: `UninitSlice` prevent writing uninitialized memory
        unsafe { self.spare_capacity_mut() }.into()
    }
}

#[cfg(feature = "inlined")]
impl<S: Slice<Item = u8> + Subsliceable + ?Sized, L: Layout> bytes::Buf
    for crate::inlined::SmallSlice<S, L>
{
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.to_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

#[cfg(feature = "inlined")]
impl<S: Slice<Item = u8> + Subsliceable + ?Sized, L: Layout> bytes::Buf
    for crate::inlined::SmallArcSlice<S, L>
{
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.to_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self._advance(cnt);
    }
}
