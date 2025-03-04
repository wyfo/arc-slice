use alloc::{borrow::Cow, boxed::Box, string::String, vec::Vec};
use core::{
    any::Any,
    borrow::Borrow,
    cmp,
    convert::Infallible,
    fmt,
    hash::{Hash, Hasher},
    mem::MaybeUninit,
    ops::{Deref, RangeBounds},
    str::{FromStr, Utf8Error},
};

use crate::{
    buffer::{Buffer, StringBuffer},
    layout::{Compact, Layout, Plain},
    macros::is,
    utils::offset_len,
    ArcBytes,
};

#[repr(transparent)]
pub(crate) struct StringBufWrapper<B>(pub(crate) B);

impl<B: StringBuffer> Buffer<u8> for StringBufWrapper<B> {
    fn as_slice(&self) -> &[u8] {
        self.0.as_str().as_bytes()
    }

    fn try_into_static(self) -> Result<&'static [u8], Self>
    where
        Self: Sized,
    {
        self.0.try_into_static().map(str::as_bytes).map_err(Self)
    }

    fn try_into_vec(self) -> Result<Vec<u8>, Self>
    where
        Self: Sized,
    {
        self.0
            .try_into_string()
            .map(String::into_bytes)
            .map_err(Self)
    }
}

#[repr(transparent)]
pub struct ArcStr<L: Layout = Compact>(ArcBytes<L>);

impl<L: Layout> ArcStr<L> {
    #[inline]
    pub fn new<B: StringBuffer>(buffer: B) -> Self {
        Self::with_metadata(buffer, ())
    }

    #[cfg(not(all(loom, test)))]
    #[inline]
    pub const fn new_static(s: &'static str) -> Self {
        unsafe { Self::from_utf8_unchecked(ArcBytes::new_static(s.as_bytes())) }
    }

    #[inline]
    pub fn with_metadata<B: StringBuffer, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Self {
        let buffer = StringBufWrapper(buffer);
        unsafe { Self::from_utf8_unchecked(ArcBytes::with_metadata(buffer, metadata)) }
    }

    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn from_str(s: &str) -> Self {
        unsafe { Self::from_utf8_unchecked(ArcBytes::from_slice(s.as_bytes())) }
    }

    #[inline]
    pub fn from_utf8(bytes: ArcBytes<L>) -> Result<Self, FromUtf8Error<ArcBytes<L>>> {
        match core::str::from_utf8(bytes.as_slice()) {
            Ok(_) => Ok(Self(bytes)),
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

    #[inline]
    pub const fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(self.0.as_slice()) }
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        check_char_boundary(self, len);
        self.0.truncate(len);
    }

    #[inline]
    pub fn advance(&mut self, offset: usize) {
        check_char_boundary(self, offset);
        self.0.advance(offset);
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
    pub fn into_string(self) -> String {
        unsafe { String::from_utf8_unchecked(self.0.into_vec()) }
    }

    #[inline]
    pub fn into_cow(self) -> Cow<'static, str> {
        unsafe {
            match self.0.into_cow() {
                Cow::Borrowed(s) => Cow::Borrowed(core::str::from_utf8_unchecked(s)),
                Cow::Owned(s) => Cow::Owned(String::from_utf8_unchecked(s)),
            }
        }
    }

    #[inline]
    pub fn get_metadata<M: Any>(&self) -> Option<&M> {
        self.0.get_metadata()
    }

    #[inline]
    pub fn downcast_buffer<B: StringBuffer>(self) -> Result<B, Self> {
        if is!(B, &'static str) {
            let mut buffer = MaybeUninit::<B>::uninit();
            let slice = self.0.downcast_buffer::<&'static [u8]>().map_err(Self)?;
            let buffer_ptr = buffer.as_mut_ptr().cast::<&'static str>();
            unsafe { buffer_ptr.write(core::str::from_utf8_unchecked(slice)) };
            return Ok(unsafe { buffer.assume_init() });
        }
        if is!(B, String) {
            let mut buffer = MaybeUninit::<B>::uninit();
            let vec = self.0.downcast_buffer::<Vec<u8>>().map_err(Self)?;
            let buffer_ptr = buffer.as_mut_ptr().cast::<String>();
            unsafe { buffer_ptr.write(String::from_utf8_unchecked(vec)) };
            return Ok(unsafe { buffer.assume_init() });
        }
        self.0
            .downcast_buffer::<StringBufWrapper<B>>()
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
    pub fn with_layout<L2: Layout>(self) -> ArcStr<L2> {
        ArcStr(self.0.with_layout())
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
        self.as_slice().hash(state);
    }
}

impl<L: Layout> Borrow<str> for ArcStr<L> {
    #[inline]
    fn borrow(&self) -> &str {
        self
    }
}

impl<L: Layout> Clone for ArcStr<L> {
    #[inline]
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(not(all(loom, test)))]
impl<L: Layout> Default for ArcStr<L> {
    #[inline]
    fn default() -> Self {
        Self::new_static("")
    }
}

impl<L: Layout> fmt::Debug for ArcStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl<L: Layout> fmt::Display for ArcStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
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

impl From<ArcStr<Compact>> for ArcStr<Plain> {
    fn from(value: ArcStr<Compact>) -> Self {
        value.with_layout()
    }
}

impl From<ArcStr<Plain>> for ArcStr<Compact> {
    fn from(value: ArcStr<Plain>) -> Self {
        value.with_layout()
    }
}

macro_rules! std_impl {
    ($($ty:ty),*) => {$(
        impl<L: Layout> From<$ty> for ArcStr<L> {

            #[inline]
            fn from(value: $ty) -> Self {
                Self::new(value)
            }
        }
    )*};
}
std_impl!(&'static str, Box<str>, String, Cow<'static, str>);

impl<L: Layout> From<ArcStr<L>> for String {
    #[inline]
    fn from(value: ArcStr<L>) -> Self {
        value.into_string()
    }
}

impl<L: Layout> From<ArcStr<L>> for Cow<'static, str> {
    #[inline]
    fn from(value: ArcStr<L>) -> Self {
        value.into_cow()
    }
}

impl<L: Layout> FromStr for ArcStr<L> {
    type Err = Infallible;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(ArcBytes::from_slice(s.as_bytes())))
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

pub struct FromUtf8Error<B> {
    pub(crate) bytes: B,
    pub(crate) error: Utf8Error,
}

impl<B> FromUtf8Error<B> {
    pub fn as_bytes(&self) -> &B {
        &self.bytes
    }

    pub fn into_bytes(self) -> B {
        self.bytes
    }

    pub fn error(&self) -> Utf8Error {
        self.error
    }
}

impl<B: fmt::Debug> fmt::Debug for FromUtf8Error<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FromUtf8Error")
            .field("bytes", &self.bytes)
            .field("error", &self.error)
            .finish()
    }
}

impl<B> fmt::Display for FromUtf8Error<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;
    impl<B: fmt::Debug> std::error::Error for FromUtf8Error<B> {}
};

pub(crate) fn check_char_boundary(s: &str, offset: usize) {
    #[cold]
    fn panic_not_a_char_boundary() -> ! {
        panic!("not a char boundary")
    }
    if !s.is_char_boundary(offset) {
        panic_not_a_char_boundary();
    }
}
