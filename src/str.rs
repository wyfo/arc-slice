use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    any::Any,
    borrow::Borrow,
    cmp,
    convert::Infallible,
    fmt,
    hash::{Hash, Hasher},
    ops::{Deref, RangeBounds},
    str::FromStr,
};

#[cfg(feature = "raw-buffer")]
use crate::buffer::RawStringBuffer;
use crate::{
    buffer::{BorrowMetadata, StringBuffer, StringBufferWrapper},
    error::FromUtf8Error,
    layout::{AnyBufferLayout, DefaultLayout, FromLayout, Layout, StaticLayout},
    macros::is,
    utils::{offset_len, try_transmute},
    ArcBytes, ArcBytesBorrow,
};

pub struct ArcStr<L: Layout = DefaultLayout>(ArcBytes<L>);

impl<L: Layout> ArcStr<L> {
    #[inline]
    pub fn new(s: &str) -> Self {
        unsafe { Self::from_utf8_unchecked(ArcBytes::new(s.as_bytes())) }
    }

    #[cfg(feature = "const-slice")]
    #[inline]
    pub const fn from_utf8(bytes: ArcBytes<L>) -> Result<Self, FromUtf8Error<ArcBytes<L>>> {
        let slice = unsafe { core::slice::from_raw_parts(bytes.start.as_ptr(), bytes.length) };
        match core::str::from_utf8(slice) {
            Ok(_) => Ok(unsafe { Self::from_utf8_unchecked(bytes) }),
            Err(error) => Err(FromUtf8Error { bytes, error }),
        }
    }

    #[cfg(not(feature = "const-slice"))]
    #[inline]
    pub fn from_utf8(bytes: ArcBytes<L>) -> Result<Self, FromUtf8Error<ArcBytes<L>>> {
        match core::str::from_utf8(bytes.as_slice()) {
            Ok(_) => Ok(unsafe { Self::from_utf8_unchecked(bytes) }),
            Err(error) => Err(FromUtf8Error { bytes, error }),
        }
    }

    /// # Safety
    ///
    /// Bytes must be valid UTF-8.
    #[inline]
    pub const unsafe fn from_utf8_unchecked(bytes: ArcBytes<L>) -> Self {
        Self(bytes)
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub const fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }

    #[cfg(feature = "const-slice")]
    #[inline]
    pub const fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(self.0.as_slice()) }
    }

    #[cfg(not(feature = "const-slice"))]
    #[inline]
    pub fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(self.0.as_slice()) }
    }

    #[inline]
    pub fn borrow(&self, range: impl RangeBounds<usize>) -> ArcStrBorrow<L> {
        let (offset, len) = offset_len(self.len(), range);
        check_char_boundary(self, offset);
        check_char_boundary(self, offset + len);
        ArcStrBorrow(unsafe { self.0.borrow_impl(offset, len) })
    }

    #[inline]
    pub fn borrow_from_ref(&self, subset: &str) -> ArcStrBorrow<L> {
        ArcStrBorrow(self.0.borrow_from_ref(subset.as_bytes()))
    }

    #[inline]
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self {
        let (offset, len) = offset_len(self.len(), range);
        check_char_boundary(self, offset);
        check_char_boundary(self, offset + len);
        unsafe { Self::from_utf8_unchecked(self.0.subslice_impl(offset, len)) }
    }

    #[inline]
    pub fn subslice_from_ref(&self, subset: &str) -> Self {
        unsafe { Self::from_utf8_unchecked(self.0.subslice_from_ref(subset.as_bytes())) }
    }

    #[inline]
    pub fn advance(&mut self, offset: usize) {
        check_char_boundary(self, offset);
        self.0.advance(offset);
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        check_char_boundary(self, len);
        self.0.truncate(len);
    }

    #[inline]
    #[must_use = "consider `ArcString::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self {
        check_char_boundary(self, at);
        unsafe { Self::from_utf8_unchecked(self.0.split_off(at)) }
    }

    #[inline]
    #[must_use = "consider `ArcString::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self {
        check_char_boundary(self, at);
        unsafe { Self::from_utf8_unchecked(self.0.split_to(at)) }
    }

    #[inline]
    pub fn is_unique(&self) -> bool {
        self.0.is_unique()
    }

    #[inline]
    pub fn metadata<M: Any>(&self) -> Option<&M> {
        self.0.metadata()
    }

    #[inline]
    pub fn try_into_buffer<B: StringBuffer>(self) -> Result<B, Self> {
        if is!(B, &'static str) {
            let slice = self.0.try_into_buffer::<&'static [u8]>().map_err(Self)?;
            return Ok(try_transmute(unsafe { core::str::from_utf8_unchecked(slice) }).unwrap());
        } else if is!(B, Box<str>) {
            let boxed_slice = self.0.try_into_buffer::<Box<[u8]>>().map_err(Self)?;
            let string = unsafe { String::from_utf8_unchecked(boxed_slice.into()) };
            return Ok(try_transmute(string.into_boxed_str()).unwrap());
        } else if is!(B, String) {
            let vec = self.0.try_into_buffer::<Vec<u8>>().map_err(Self)?;
            return Ok(try_transmute(unsafe { String::from_utf8_unchecked(vec) }).unwrap());
        }
        self.0
            .try_into_buffer::<StringBufferWrapper<B>>()
            .map_err(Self)
            .map(|s| s.0)
    }

    #[inline]
    pub fn as_slice(&self) -> &ArcBytes<L> {
        &self.0
    }

    #[inline]
    pub fn into_slice(self) -> ArcBytes<L> {
        self.0
    }

    #[inline]
    pub fn with_layout<L2: Layout + FromLayout<L>>(self) -> ArcStr<L2> {
        ArcStr(self.0.with_layout())
    }

    pub fn drop_with_unique_hint(self) {
        self.0.drop_with_unique_hint();
    }
}

impl<L: StaticLayout> ArcStr<L> {
    pub const fn new_static(s: &'static str) -> Self {
        unsafe { Self::from_utf8_unchecked(ArcBytes::new_static(s.as_bytes())) }
    }
}

impl<L: AnyBufferLayout> ArcStr<L> {
    pub fn from_buffer<B: StringBuffer>(buffer: B) -> Self {
        Self::from_buffer_with_metadata(buffer, ())
    }

    pub fn from_buffer_with_metadata<B: StringBuffer, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Self {
        if is!(M, ()) {
            return buffer.into_arc_str();
        }
        let bytes = ArcBytes::from_buffer_with_metadata(StringBufferWrapper(buffer), metadata);
        unsafe { Self::from_utf8_unchecked(bytes) }
    }

    #[inline]
    pub fn from_buffer_with_borrowed_metadata<B: StringBuffer + BorrowMetadata>(buffer: B) -> Self {
        let bytes = ArcBytes::from_buffer_with_borrowed_metadata(StringBufferWrapper(buffer));
        unsafe { Self::from_utf8_unchecked(bytes) }
    }

    #[cfg(feature = "raw-buffer")]
    #[inline]
    pub fn from_raw_buffer<B: RawStringBuffer>(buffer: B) -> Self {
        let bytes = ArcBytes::from_raw_buffer(StringBufferWrapper(buffer));
        unsafe { Self::from_utf8_unchecked(bytes) }
    }

    #[cfg(feature = "raw-buffer")]
    #[inline]
    pub fn from_raw_buffer_and_borrowed_metadata<B: RawStringBuffer + BorrowMetadata>(
        buffer: B,
    ) -> Self {
        let bytes = ArcBytes::from_raw_buffer_and_borrowed_metadata(StringBufferWrapper(buffer));
        unsafe { Self::from_utf8_unchecked(bytes) }
    }
}

impl<L: Layout> Clone for ArcStr<L> {
    #[inline]
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<L: Layout> Deref for ArcStr<L> {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl<L: Layout> AsRef<str> for ArcStr<L> {
    #[inline]
    fn as_ref(&self) -> &str {
        self
    }
}

impl<L: Layout> AsRef<[u8]> for ArcStr<L> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<L: Layout> Hash for ArcStr<L> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_str().hash(state);
    }
}

impl<L: Layout> Borrow<str> for ArcStr<L> {
    #[inline]
    fn borrow(&self) -> &str {
        self
    }
}

impl<L: StaticLayout> Default for ArcStr<L> {
    #[inline]
    fn default() -> Self {
        Self::new_static("")
    }
}

impl<L: Layout> fmt::Debug for ArcStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<L: Layout> fmt::Display for ArcStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<L: Layout> PartialEq for ArcStr<L> {
    fn eq(&self, other: &ArcStr<L>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<L: Layout> Eq for ArcStr<L> {}

impl<L: Layout> PartialOrd for ArcStr<L> {
    fn partial_cmp(&self, other: &ArcStr<L>) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<L: Layout> Ord for ArcStr<L> {
    fn cmp(&self, other: &ArcStr<L>) -> cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl<L: Layout> PartialEq<str> for ArcStr<L> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl<L: Layout> PartialEq<ArcStr<L>> for str {
    fn eq(&self, other: &ArcStr<L>) -> bool {
        *other == *self
    }
}

impl<L: Layout> PartialEq<String> for ArcStr<L> {
    fn eq(&self, other: &String) -> bool {
        *self == other[..]
    }
}

impl<L: Layout> PartialEq<ArcStr<L>> for String {
    fn eq(&self, other: &ArcStr<L>) -> bool {
        *other == *self
    }
}

impl<L: Layout> PartialEq<ArcStr<L>> for &str {
    fn eq(&self, other: &ArcStr<L>) -> bool {
        *other == *self
    }
}

impl<'a, L: Layout, O: ?Sized> PartialEq<&'a O> for ArcStr<L>
where
    ArcStr<L>: PartialEq<O>,
{
    fn eq(&self, other: &&'a O) -> bool {
        *self == **other
    }
}

impl<L: AnyBufferLayout> From<Box<str>> for ArcStr<L> {
    fn from(value: Box<str>) -> Self {
        value.into_arc_str()
    }
}

impl<L: AnyBufferLayout> From<String> for ArcStr<L> {
    fn from(value: String) -> Self {
        value.into_arc_str()
    }
}

impl<L: Layout> FromStr for ArcStr<L> {
    type Err = Infallible;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(ArcBytes::new(s.as_bytes())))
    }
}

impl<L: Layout> TryFrom<ArcBytes<L>> for ArcStr<L> {
    type Error = FromUtf8Error<ArcBytes<L>>;

    #[inline]
    fn try_from(value: ArcBytes<L>) -> Result<Self, Self::Error> {
        Self::from_utf8(value)
    }
}

impl<L: Layout> From<ArcStr<L>> for ArcBytes<L> {
    #[inline]
    fn from(value: ArcStr<L>) -> Self {
        value.into_slice()
    }
}

#[derive(Clone, Copy)]
pub struct ArcStrBorrow<'a, L: Layout = DefaultLayout>(ArcBytesBorrow<'a, L>);

impl<L: Layout> Deref for ArcStrBorrow<'_, L> {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}

impl<L: Layout> fmt::Debug for ArcStrBorrow<'_, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<L: Layout> ArcStrBorrow<'_, L> {
    #[inline]
    pub fn to_owned(self) -> ArcStr<L> {
        unsafe { ArcStr::from_utf8_unchecked(self.0.to_owned()) }
    }
}

pub(crate) fn check_char_boundary(s: &str, offset: usize) {
    #[cold]
    fn panic_not_a_char_boundary() -> ! {
        panic!("not a char boundary")
    }
    if !s.is_char_boundary(offset) {
        panic_not_a_char_boundary();
    }
}
