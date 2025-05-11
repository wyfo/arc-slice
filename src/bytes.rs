use crate::{
    layout::{Layout, LayoutMut},
    ArcBytes, ArcBytesMut, ArcStr,
};

impl<L: Layout> bytes::Buf for ArcBytes<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

impl<L: LayoutMut, const UNIQUE: bool> bytes::Buf for ArcBytesMut<L, UNIQUE> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

unsafe impl<L: LayoutMut, const UNIQUE: bool> bytes::BufMut for ArcBytesMut<L, UNIQUE> {
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

impl<L: Layout> bytes::Buf for ArcStr<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

#[cfg(feature = "inlined")]
impl<L: Layout> bytes::Buf for crate::inlined::SmallBytes<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

#[cfg(feature = "inlined")]
impl<L: Layout> bytes::Buf for crate::inlined::SmallArcBytes<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self._advance(cnt);
    }
}

#[cfg(feature = "inlined")]
impl<L: Layout> bytes::Buf for crate::inlined::SmallStr<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self.advance(cnt);
    }
}

#[cfg(feature = "inlined")]
impl<L: Layout> bytes::Buf for crate::inlined::SmallArcStr<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_slice()
    }

    fn advance(&mut self, cnt: usize) {
        self._advance(cnt);
    }
}
