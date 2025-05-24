use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    borrow::Borrow,
    cmp, fmt,
    ops::{Deref, RangeBounds},
};

use arc_slice::{buffer::AsRefBuffer, ArcBytes};

use crate::{Buf, BytesMut};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, bytemuck::TransparentWrapper)]
#[repr(transparent)]
pub struct Bytes(ArcBytes);

impl Default for Bytes {
    fn default() -> Bytes {
        Self(ArcBytes::from_slice(&[]))
    }
}

impl Bytes {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn from_static(bytes: &'static [u8]) -> Self {
        Self(ArcBytes::from_static(bytes))
    }

    pub fn from_owner<T>(owner: T) -> Self
    where
        T: AsRef<[u8]> + Send + 'static,
    {
        Self(ArcBytes::from_buffer(AsRefBuffer::<_, false>(owner)))
    }

    pub const fn len(&self) -> usize {
        self.0.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_unique(&self) -> bool {
        self.0.is_unique()
    }

    pub fn copy_from_slice(data: &[u8]) -> Self {
        Self(ArcBytes::from_slice(data))
    }

    pub fn slice(&self, range: impl RangeBounds<usize>) -> Self {
        Self(self.0.subslice(range))
    }

    pub fn slice_ref(&self, subset: &[u8]) -> Self {
        if subset.is_empty() {
            return Self::new();
        }
        Self(self.0.subslice_from_ref(subset))
    }

    pub fn split_off(&mut self, at: usize) -> Self {
        Self(self.0.split_off(at))
    }

    pub fn split_to(&mut self, at: usize) -> Self {
        Self(self.0.split_to(at))
    }

    pub fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }

    pub fn clear(&mut self) {
        self.truncate(0);
    }

    pub fn try_into_mut(self) -> Result<BytesMut, Self> {
        self.0.try_into_mut().map(BytesMut::from).map_err(Self)
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::LowerHex for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::UpperHex for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Buf for Bytes {
    fn remaining(&self) -> usize {
        self.len()
    }

    fn chunk(&self) -> &[u8] {
        self
    }

    fn advance(&mut self, cnt: usize) {
        self.0.advance(cnt);
    }

    fn copy_to_bytes(&mut self, len: usize) -> Self {
        self.split_to(len)
    }
}

impl Deref for Bytes {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for Bytes {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl Borrow<[u8]> for Bytes {
    fn borrow(&self) -> &[u8] {
        self
    }
}

impl IntoIterator for Bytes {
    type Item = u8;
    type IntoIter = crate::buf::IntoIter<Bytes>;

    fn into_iter(self) -> Self::IntoIter {
        crate::buf::IntoIter::new(self)
    }
}

impl<'a> IntoIterator for &'a Bytes {
    type Item = &'a u8;
    type IntoIter = core::slice::Iter<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl FromIterator<u8> for Bytes {
    fn from_iter<T: IntoIterator<Item = u8>>(into_iter: T) -> Self {
        Vec::from_iter(into_iter).into()
    }
}

impl PartialEq<[u8]> for Bytes {
    fn eq(&self, other: &[u8]) -> bool {
        self[..] == *other
    }
}

impl PartialOrd<[u8]> for Bytes {
    fn partial_cmp(&self, other: &[u8]) -> Option<cmp::Ordering> {
        self[..].partial_cmp(other)
    }
}

impl PartialEq<Bytes> for [u8] {
    fn eq(&self, other: &Bytes) -> bool {
        *other == *self
    }
}

impl PartialOrd<Bytes> for [u8] {
    fn partial_cmp(&self, other: &Bytes) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self, other)
    }
}

impl PartialEq<str> for Bytes {
    fn eq(&self, other: &str) -> bool {
        self[..] == *other.as_bytes()
    }
}

impl PartialOrd<str> for Bytes {
    fn partial_cmp(&self, other: &str) -> Option<cmp::Ordering> {
        self[..].partial_cmp(other.as_bytes())
    }
}

impl PartialEq<Bytes> for str {
    fn eq(&self, other: &Bytes) -> bool {
        *other == *self
    }
}

impl PartialOrd<Bytes> for str {
    fn partial_cmp(&self, other: &Bytes) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self.as_bytes(), other)
    }
}

impl PartialEq<Vec<u8>> for Bytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        *self == other[..]
    }
}

impl PartialOrd<Vec<u8>> for Bytes {
    fn partial_cmp(&self, other: &Vec<u8>) -> Option<cmp::Ordering> {
        self[..].partial_cmp(&other[..])
    }
}

impl PartialEq<Bytes> for Vec<u8> {
    fn eq(&self, other: &Bytes) -> bool {
        *other == *self
    }
}

impl PartialOrd<Bytes> for Vec<u8> {
    fn partial_cmp(&self, other: &Bytes) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self, other)
    }
}

impl PartialEq<String> for Bytes {
    fn eq(&self, other: &String) -> bool {
        *self == other[..]
    }
}

impl PartialOrd<String> for Bytes {
    fn partial_cmp(&self, other: &String) -> Option<cmp::Ordering> {
        self[..].partial_cmp(other.as_bytes())
    }
}

impl PartialEq<Bytes> for String {
    fn eq(&self, other: &Bytes) -> bool {
        *other == *self
    }
}

impl PartialOrd<Bytes> for String {
    fn partial_cmp(&self, other: &Bytes) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self.as_bytes(), other)
    }
}

impl PartialEq<Bytes> for &[u8] {
    fn eq(&self, other: &Bytes) -> bool {
        *other == *self
    }
}

impl PartialOrd<Bytes> for &[u8] {
    fn partial_cmp(&self, other: &Bytes) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self, other)
    }
}

impl PartialEq<Bytes> for &str {
    fn eq(&self, other: &Bytes) -> bool {
        *other == *self
    }
}

impl PartialOrd<Bytes> for &str {
    fn partial_cmp(&self, other: &Bytes) -> Option<cmp::Ordering> {
        <[u8] as PartialOrd<[u8]>>::partial_cmp(self.as_bytes(), other)
    }
}

impl<'a, T: ?Sized> PartialEq<&'a T> for Bytes
where
    Bytes: PartialEq<T>,
{
    fn eq(&self, other: &&'a T) -> bool {
        *self == **other
    }
}

impl<'a, T: ?Sized> PartialOrd<&'a T> for Bytes
where
    Bytes: PartialOrd<T>,
{
    fn partial_cmp(&self, other: &&'a T) -> Option<cmp::Ordering> {
        self.partial_cmp(&**other)
    }
}

impl From<&'static [u8]> for Bytes {
    fn from(slice: &'static [u8]) -> Bytes {
        Bytes::from_static(slice)
    }
}

impl From<&'static str> for Bytes {
    fn from(slice: &'static str) -> Bytes {
        Bytes::from_static(slice.as_bytes())
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(vec: Vec<u8>) -> Bytes {
        Self(vec.into())
    }
}

impl From<Box<[u8]>> for Bytes {
    fn from(slice: Box<[u8]>) -> Bytes {
        Self(slice.into())
    }
}

impl From<String> for Bytes {
    fn from(s: String) -> Bytes {
        Bytes::from(s.into_bytes())
    }
}

impl From<Bytes> for Vec<u8> {
    fn from(bytes: Bytes) -> Vec<u8> {
        bytes
            .0
            .try_into_buffer::<Vec<u8>>()
            .unwrap_or_else(|bytes| bytes[..].to_vec())
    }
}

impl From<Bytes> for ArcBytes {
    fn from(value: Bytes) -> ArcBytes {
        value.0
    }
}

impl<'a> From<&'a Bytes> for &'a ArcBytes {
    fn from(value: &'a Bytes) -> &'a ArcBytes {
        &value.0
    }
}

impl<'a> From<&'a mut Bytes> for &'a mut ArcBytes {
    fn from(value: &'a mut Bytes) -> &'a mut ArcBytes {
        &mut value.0
    }
}

impl From<ArcBytes> for Bytes {
    fn from(value: ArcBytes) -> Bytes {
        Bytes(value)
    }
}
