#[cfg(feature = "serde")]
use alloc::string::String;
use alloc::{boxed::Box, vec::Vec};
use core::convert::Infallible;

use bstr::{BStr, BString, ByteSlice};

#[cfg(feature = "serde")]
use crate::buffer::Deserializable;
use crate::{
    buffer::{
        Buffer, BufferMut, Concatenable, Emptyable, Extendable, Slice, Subsliceable, Zeroable,
    },
    error::TryReserveError,
};

unsafe impl Slice for BStr {
    type Item = u8;
    type Vec = BString;

    fn to_slice(&self) -> &[Self::Item] {
        self
    }
    unsafe fn to_slice_mut(&mut self) -> &mut [Self::Item] {
        self
    }
    fn into_boxed_slice(self: Box<Self>) -> Box<[Self::Item]> {
        self.into()
    }
    fn into_vec(vec: Self::Vec) -> Vec<Self::Item> {
        vec.into()
    }

    unsafe fn from_slice_unchecked(slice: &[Self::Item]) -> &Self {
        slice.as_bstr()
    }
    unsafe fn from_slice_mut_unchecked(slice: &mut [Self::Item]) -> &mut Self {
        slice.as_bstr_mut()
    }
    unsafe fn from_boxed_slice_unchecked(boxed: Box<[Self::Item]>) -> Box<Self> {
        boxed.into()
    }
    unsafe fn from_vec_unchecked(vec: Vec<Self::Item>) -> Self::Vec {
        vec.into()
    }

    type TryFromSliceError = Infallible;
    fn try_from_slice(slice: &[Self::Item]) -> Result<&Self, Self::TryFromSliceError> {
        Ok(slice.as_bstr())
    }
    fn try_from_slice_mut(slice: &mut [Self::Item]) -> Result<&mut Self, Self::TryFromSliceError> {
        Ok(slice.as_bstr_mut())
    }
}

unsafe impl Emptyable for BStr {}

unsafe impl Zeroable for BStr {}

unsafe impl Subsliceable for BStr {
    unsafe fn check_subslice(&self, _start: usize, _end: usize) {}
}

unsafe impl Concatenable for BStr {}

unsafe impl Extendable for BStr {}

#[cfg(feature = "serde")]
impl Deserializable for BStr {
    fn deserialize<'de, D: serde::Deserializer<'de>, V: serde::de::Visitor<'de>>(
        deserializer: D,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        deserializer.deserialize_byte_buf(visitor)
    }
    fn expecting(f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "a byte string")
    }
    fn deserialize_from_bytes<E: serde::de::Error>(bytes: &[u8]) -> Result<&Self, E> {
        Ok(bytes.into())
    }
    fn deserialize_from_byte_buf<E: serde::de::Error>(bytes: Vec<u8>) -> Result<Self::Vec, E> {
        Ok(bytes.into())
    }
    fn deserialize_from_str<E: serde::de::Error>(s: &str) -> Result<&Self, E> {
        Ok(s.into())
    }
    fn deserialize_from_string<E: serde::de::Error>(s: String) -> Result<Self::Vec, E> {
        Ok(s.into())
    }
    fn try_deserialize_from_seq() -> bool {
        false
    }
}

impl Buffer<BStr> for BString {
    fn as_slice(&self) -> &BStr {
        self.as_bstr()
    }
}

unsafe impl BufferMut<BStr> for BString {
    fn as_mut_slice(&mut self) -> &mut BStr {
        self.as_bstr_mut()
    }

    fn capacity(&self) -> usize {
        (**self).capacity()
    }

    unsafe fn set_len(&mut self, len: usize) -> bool {
        unsafe { (**self).set_len(len) };
        true
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        BufferMut::try_reserve(&mut **self, additional)
    }
}
