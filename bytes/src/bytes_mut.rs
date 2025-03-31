use alloc::{string::String, vec::Vec};
use core::{
    borrow::{Borrow, BorrowMut},
    cmp, fmt,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    ptr,
};

use arc_slice::{ArcBytes, ArcBytesMut};

use crate::{buf::UninitSlice, Buf, BufMut, Bytes, TryGetError};

#[derive(Default, PartialEq, Eq, PartialOrd, Ord, Hash, bytemuck::TransparentWrapper)]
#[repr(transparent)]
pub struct BytesMut(ArcBytesMut);

impl BytesMut {
    #[cfg(feature = "serde")]
    pub(crate) fn from_vec(vec: Vec<u8>) -> Self {
        Self(vec.into())
    }

    pub fn with_capacity(capacity: usize) -> BytesMut {
        Self(Vec::with_capacity(capacity).into())
    }

    pub fn new() -> BytesMut {
        Self::with_capacity(0)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }

    pub fn freeze(self) -> Bytes {
        Bytes::from(self.0.freeze())
    }

    pub fn zeroed(len: usize) -> BytesMut {
        Self(alloc::vec![0; len].into())
    }

    #[must_use = "consider BytesMut::truncate if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> BytesMut {
        Self(self.0.split_off(at))
    }

    #[must_use = "consider BytesMut::clear if you don't need the other half"]
    pub fn split(&mut self) -> BytesMut {
        self.split_to(self.len())
    }

    #[must_use = "consider BytesMut::advance if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> BytesMut {
        Self(self.0.split_to(at))
    }

    pub fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }

    pub fn clear(&mut self) {
        self.truncate(0);
    }

    pub fn resize(&mut self, new_len: usize, value: u8) {
        let additional = if let Some(additional) = new_len.checked_sub(self.len()) {
            additional
        } else {
            self.truncate(new_len);
            return;
        };

        if additional == 0 {
            return;
        }

        self.0.try_reserve(additional).unwrap();
        let dst = unsafe { self.0.spare_capacity_mut() }.as_mut_ptr();
        // SAFETY: `spare_capacity_mut` returns a valid, properly aligned pointer and we've
        // reserved enough space to write `additional` bytes.
        unsafe { ptr::write_bytes(dst, value, additional) };

        // SAFETY: There are at least `new_len` initialized bytes in the buffer so no
        // uninitialized bytes are being exposed.
        unsafe { self.set_len(new_len) };
    }

    /// # Safety
    ///
    /// First `len` items of the slice must be initialized.
    pub unsafe fn set_len(&mut self, len: usize) {
        unsafe { self.0.set_len(len) }
    }

    pub fn reserve(&mut self, additional: usize) {
        if self.0.try_reserve(additional).is_err() {
            let mut new = BytesMut::with_capacity(self.len() + additional);
            new.extend_from_slice(self);
            *self = new;
        }
    }

    pub fn try_reclaim(&mut self, additional: usize) -> bool {
        self.0.try_reclaim(additional)
    }

    pub fn extend_from_slice(&mut self, extend: &[u8]) {
        if self.0.try_extend_from_slice(extend).is_err() {
            *self = BytesMut([self.as_ref(), extend].concat().into())
        }
    }

    pub fn unsplit(&mut self, other: BytesMut) {
        if self.is_empty() {
            *self = other;
            return;
        }
        if let Err(other) = self.0.try_unsplit(other.0) {
            #[cold]
            fn realloc(bytes: &mut BytesMut, other: ArcBytesMut) {
                bytes.extend_from_slice(&other);
            }
            realloc(self, other);
        }
    }

    pub fn spare_capacity_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY: implementation of `ArcSliceMut` allows writing uninitialized
        // bytes to spare capacity when the underlying buffer is a `Vec`,
        // as the buffer is then stored by raw parts
        unsafe { self.0.spare_capacity_mut() }
    }
}

impl Buf for BytesMut {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self
    }

    fn advance(&mut self, cnt: usize) {
        self.0.advance(cnt);
    }

    fn copy_to_bytes(&mut self, len: usize) -> Bytes {
        self.split_to(len).freeze()
    }
}

unsafe impl BufMut for BytesMut {
    #[inline]
    fn remaining_mut(&self) -> usize {
        usize::MAX - self.len()
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        let remaining = self.capacity() - self.len();
        if cnt > remaining {
            super::panic_advance(&TryGetError {
                requested: cnt,
                available: remaining,
            });
        }
        // Addition won't overflow since it is at most `self.cap`.
        unsafe {
            self.set_len(self.len() + cnt);
        }
    }

    #[inline]
    fn chunk_mut(&mut self) -> &mut UninitSlice {
        if self.capacity() == self.len() {
            self.reserve(64);
        }
        unsafe { self.spare_capacity_mut() }.into()
    }

    fn put<T: Buf>(&mut self, mut src: T)
    where
        Self: Sized,
    {
        while src.has_remaining() {
            let s = src.chunk();
            let l = s.len();
            self.extend_from_slice(s);
            src.advance(l);
        }
    }

    fn put_slice(&mut self, src: &[u8]) {
        self.extend_from_slice(src);
    }

    fn put_bytes(&mut self, val: u8, cnt: usize) {
        self.reserve(cnt);
        unsafe {
            let dst = self.spare_capacity_mut();
            // Reserved above
            debug_assert!(dst.len() >= cnt);

            ptr::write_bytes(dst.as_mut_ptr(), val, cnt);

            self.advance_mut(cnt);
        }
    }
}

impl AsRef<[u8]> for BytesMut {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl Deref for BytesMut {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}

impl AsMut<[u8]> for BytesMut {
    #[inline]
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.as_mut_slice()
    }
}

impl DerefMut for BytesMut {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        self.as_mut()
    }
}

impl<'a> From<&'a [u8]> for BytesMut {
    fn from(src: &'a [u8]) -> BytesMut {
        BytesMut(src.to_vec().into())
    }
}

impl<'a> From<&'a str> for BytesMut {
    fn from(src: &'a str) -> BytesMut {
        BytesMut::from(src.as_bytes())
    }
}

impl From<BytesMut> for Bytes {
    fn from(src: BytesMut) -> Bytes {
        src.freeze()
    }
}

impl Borrow<[u8]> for BytesMut {
    fn borrow(&self) -> &[u8] {
        self.as_ref()
    }
}

impl BorrowMut<[u8]> for BytesMut {
    fn borrow_mut(&mut self) -> &mut [u8] {
        self.as_mut()
    }
}

impl fmt::Write for BytesMut {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.remaining_mut() >= s.len() {
            self.put_slice(s.as_bytes());
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }

    #[inline]
    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::write(self, args)
    }
}

impl Clone for BytesMut {
    fn clone(&self) -> BytesMut {
        BytesMut::from(&self[..])
    }
}

impl IntoIterator for BytesMut {
    type Item = u8;
    type IntoIter = crate::buf::IntoIter<BytesMut>;

    fn into_iter(self) -> Self::IntoIter {
        crate::buf::IntoIter::new(self)
    }
}

impl<'a> IntoIterator for &'a BytesMut {
    type Item = &'a u8;
    type IntoIter = core::slice::Iter<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_ref().iter()
    }
}

impl Extend<u8> for BytesMut {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = u8>,
    {
        let iter = iter.into_iter();

        let (lower, _) = iter.size_hint();
        self.reserve(lower);

        for b in iter {
            self.put_u8(b);
        }
    }
}

impl<'a> Extend<&'a u8> for BytesMut {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = &'a u8>,
    {
        self.extend(iter.into_iter().copied());
    }
}

impl Extend<Bytes> for BytesMut {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = Bytes>,
    {
        for bytes in iter {
            self.extend_from_slice(&bytes);
        }
    }
}

impl FromIterator<u8> for BytesMut {
    fn from_iter<T: IntoIterator<Item = u8>>(into_iter: T) -> BytesMut {
        BytesMut(Vec::from_iter(into_iter).into())
    }
}

impl<'a> FromIterator<&'a u8> for BytesMut {
    fn from_iter<T: IntoIterator<Item = &'a u8>>(into_iter: T) -> BytesMut {
        BytesMut::from_iter(into_iter.into_iter().copied())
    }
}

impl PartialEq<[u8]> for BytesMut {
    fn eq(&self, other: &[u8]) -> bool {
        &**self == other
    }
}

impl PartialOrd<[u8]> for BytesMut {
    fn partial_cmp(&self, other: &[u8]) -> Option<cmp::Ordering> {
        (**self).partial_cmp(other)
    }
}

impl PartialEq<BytesMut> for [u8] {
    fn eq(&self, other: &BytesMut) -> bool {
        *other == *self
    }
}

impl PartialOrd<BytesMut> for [u8] {
    fn partial_cmp(&self, other: &BytesMut) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self, other)
    }
}

impl PartialEq<str> for BytesMut {
    fn eq(&self, other: &str) -> bool {
        &**self == other.as_bytes()
    }
}

impl PartialOrd<str> for BytesMut {
    fn partial_cmp(&self, other: &str) -> Option<cmp::Ordering> {
        (**self).partial_cmp(other.as_bytes())
    }
}

impl PartialEq<BytesMut> for str {
    fn eq(&self, other: &BytesMut) -> bool {
        *other == *self
    }
}

impl PartialOrd<BytesMut> for str {
    fn partial_cmp(&self, other: &BytesMut) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self.as_bytes(), other)
    }
}

impl PartialEq<Vec<u8>> for BytesMut {
    fn eq(&self, other: &Vec<u8>) -> bool {
        *self == other[..]
    }
}

impl PartialOrd<Vec<u8>> for BytesMut {
    fn partial_cmp(&self, other: &Vec<u8>) -> Option<cmp::Ordering> {
        (**self).partial_cmp(&other[..])
    }
}

impl PartialEq<BytesMut> for Vec<u8> {
    fn eq(&self, other: &BytesMut) -> bool {
        *other == *self
    }
}

impl PartialOrd<BytesMut> for Vec<u8> {
    fn partial_cmp(&self, other: &BytesMut) -> Option<cmp::Ordering> {
        other.partial_cmp(self)
    }
}

impl PartialEq<String> for BytesMut {
    fn eq(&self, other: &String) -> bool {
        *self == other[..]
    }
}

impl PartialOrd<String> for BytesMut {
    fn partial_cmp(&self, other: &String) -> Option<cmp::Ordering> {
        (**self).partial_cmp(other.as_bytes())
    }
}

impl PartialEq<BytesMut> for String {
    fn eq(&self, other: &BytesMut) -> bool {
        *other == *self
    }
}

impl PartialOrd<BytesMut> for String {
    fn partial_cmp(&self, other: &BytesMut) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self.as_bytes(), other)
    }
}

impl<'a, T: ?Sized> PartialEq<&'a T> for BytesMut
where
    BytesMut: PartialEq<T>,
{
    fn eq(&self, other: &&'a T) -> bool {
        *self == **other
    }
}

impl<'a, T: ?Sized> PartialOrd<&'a T> for BytesMut
where
    BytesMut: PartialOrd<T>,
{
    fn partial_cmp(&self, other: &&'a T) -> Option<cmp::Ordering> {
        self.partial_cmp(*other)
    }
}

impl PartialEq<BytesMut> for &[u8] {
    fn eq(&self, other: &BytesMut) -> bool {
        *other == *self
    }
}

impl PartialOrd<BytesMut> for &[u8] {
    fn partial_cmp(&self, other: &BytesMut) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self, other)
    }
}

impl PartialEq<BytesMut> for &str {
    fn eq(&self, other: &BytesMut) -> bool {
        *other == *self
    }
}

impl PartialOrd<BytesMut> for &str {
    fn partial_cmp(&self, other: &BytesMut) -> Option<cmp::Ordering> {
        other.partial_cmp(self)
    }
}

impl PartialEq<BytesMut> for Bytes {
    fn eq(&self, other: &BytesMut) -> bool {
        other[..] == self[..]
    }
}

impl PartialEq<Bytes> for BytesMut {
    fn eq(&self, other: &Bytes) -> bool {
        other[..] == self[..]
    }
}

impl From<BytesMut> for Vec<u8> {
    fn from(bytes: BytesMut) -> Self {
        bytes.0.into_vec()
    }
}

impl fmt::Debug for BytesMut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::LowerHex for BytesMut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::UpperHex for BytesMut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<BytesMut> for ArcBytesMut {
    fn from(value: BytesMut) -> ArcBytesMut {
        value.0
    }
}

impl<'a> From<&'a BytesMut> for &'a ArcBytesMut {
    fn from(value: &'a BytesMut) -> &'a ArcBytesMut {
        &value.0
    }
}

impl<'a> From<&'a mut BytesMut> for &'a mut ArcBytesMut {
    fn from(value: &'a mut BytesMut) -> &'a mut ArcBytesMut {
        &mut value.0
    }
}

impl From<ArcBytesMut> for BytesMut {
    fn from(value: ArcBytesMut) -> BytesMut {
        BytesMut(value)
    }
}

impl From<Bytes> for BytesMut {
    fn from(bytes: Bytes) -> BytesMut {
        bytes
            .try_into_mut()
            .unwrap_or_else(|bytes| ArcBytesMut::from(ArcBytes::from(bytes).into_vec()).into())
    }
}
