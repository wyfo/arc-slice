use crate::{layout::Layout, str::check_char_boundary, ArcSlice, ArcSliceMut, ArcStr};

impl<L: Layout> bytes::Buf for ArcSlice<u8, L> {
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

impl bytes::Buf for ArcSliceMut<u8> {
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

unsafe impl bytes::BufMut for ArcSliceMut<u8> {
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
        self.as_ref()
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
        self
    }

    fn advance(&mut self, cnt: usize) {
        match self.as_either_mut() {
            either::Either::Left(bytes) => bytes.advance(cnt),
            either::Either::Right(bytes) => bytes.advance(cnt),
        }
    }
}

#[cfg(feature = "inlined")]
impl<L: Layout> bytes::Buf for crate::inlined::SmallArcStr<L> {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self.as_ref()
    }

    fn advance(&mut self, cnt: usize) {
        check_char_boundary(self, cnt);
        match self.as_either_mut() {
            either::Either::Left(s) => s.advance(cnt),
            either::Either::Right(s) => s.advance(cnt),
        }
    }
}
