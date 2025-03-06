use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use core::{
    borrow::Borrow,
    cmp,
    convert::Infallible,
    fmt,
    hash::{Hash, Hasher},
    mem,
    mem::{size_of, ManuallyDrop, MaybeUninit},
    ops::{Deref, RangeBounds},
    ptr,
    ptr::addr_of,
    slice,
    str::FromStr,
};

use either::Either;

use crate::{
    buffer::{Buffer, StringBuffer},
    layout::{Compact, Layout, Plain},
    rust_compat::{ptr_from_mut, ptr_from_ref},
    str::{check_char_boundary, FromUtf8Error, StringBufWrapper},
    utils::{debug_slice, offset_len, panic_out_of_range},
    ArcBytes, ArcStr,
};

const INLINED_FLAG: u8 = 0x80;

pub trait InlinedLayout {
    const LEN: usize;
    type Data: Copy;
    const DEFAULT: Self::Data;
}

const COMPACT_LEN: usize = 3 * size_of::<usize>() - 2;
const PLAIN_LEN: usize = 4 * size_of::<usize>() - 2;

impl InlinedLayout for Compact {
    const LEN: usize = COMPACT_LEN;
    type Data = [MaybeUninit<u8>; COMPACT_LEN];
    const DEFAULT: Self::Data = [MaybeUninit::uninit(); COMPACT_LEN];
}

impl InlinedLayout for Plain {
    const LEN: usize = PLAIN_LEN;
    type Data = [MaybeUninit<u8>; PLAIN_LEN];
    const DEFAULT: Self::Data = [MaybeUninit::uninit(); PLAIN_LEN];
}

#[repr(C)]
pub struct SmallBytes<L: Layout> {
    #[cfg(target_endian = "big")]
    tagged_length: u8,
    data: <L as InlinedLayout>::Data,
    offset: u8,
    #[cfg(target_endian = "little")]
    tagged_length: u8,
}

impl<L: Layout> SmallBytes<L> {
    const MAX_LEN: usize = L::LEN;

    #[inline]
    pub fn new(slice: &[u8]) -> Option<Self> {
        if slice.len() > Self::MAX_LEN {
            return None;
        }
        let mut this = Self {
            data: L::DEFAULT,
            offset: 0,
            tagged_length: slice.len() as u8 | INLINED_FLAG,
        };
        let data = ptr_from_mut(&mut this.data).cast::<u8>();
        unsafe { ptr::copy_nonoverlapping(slice.as_ptr(), data, slice.len()) }
        Some(this)
    }

    #[inline(always)]
    const fn is_inlined(this: *const Self) -> bool {
        unsafe { (*addr_of!((*this).tagged_length)) & INLINED_FLAG != 0 }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        (self.tagged_length & !INLINED_FLAG) as usize
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub const fn as_slice(&self) -> &[u8] {
        let data = ptr_from_ref(&self.data).cast::<u8>();
        unsafe { slice::from_raw_parts(data.add(self.offset as usize), self.len()) }
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len < self.len() {
            self.tagged_length = len as u8 | INLINED_FLAG;
        }
    }

    #[inline]
    pub fn advance(&mut self, offset: usize) {
        if offset > self.len() {
            panic_out_of_range()
        }
        self.offset += offset as u8;
        self.tagged_length -= offset as u8;
    }

    #[inline]
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self {
        let (offset, len) = offset_len(self.len(), range);
        Self {
            offset: self.offset + offset as u8,
            tagged_length: len as u8 | INLINED_FLAG,
            ..*self
        }
    }
}

impl<L: Layout> Clone for SmallBytes<L> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Layout> Copy for SmallBytes<L> {}

impl<L: Layout> Deref for SmallBytes<L> {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<L: Layout> AsRef<[u8]> for SmallBytes<L> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl<L: Layout> Hash for SmallBytes<L> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<L: Layout> Borrow<[u8]> for SmallBytes<L> {
    #[inline]
    fn borrow(&self) -> &[u8] {
        self
    }
}

impl<L: Layout> Default for SmallBytes<L> {
    #[inline]
    fn default() -> Self {
        Self::new(&[]).unwrap()
    }
}

impl<L: Layout> fmt::Debug for SmallBytes<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self, f)
    }
}

impl<L: Layout> fmt::LowerHex for SmallBytes<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.as_slice() {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl<L: Layout> fmt::UpperHex for SmallBytes<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.as_slice() {
            write!(f, "{:02X}", b)?;
        }
        Ok(())
    }
}

impl<L: Layout> PartialEq for SmallBytes<L> {
    fn eq(&self, other: &SmallBytes<L>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<L: Layout> Eq for SmallBytes<L> {}

impl<L: Layout> PartialOrd for SmallBytes<L> {
    fn partial_cmp(&self, other: &SmallBytes<L>) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<L: Layout> Ord for SmallBytes<L> {
    fn cmp(&self, other: &SmallBytes<L>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

pub struct SmallArcBytes<L: Layout = Compact>(Inner<L>);

#[repr(C)]
union Inner<L: Layout> {
    small: SmallBytes<L>,
    arc: ManuallyDrop<ArcBytes<L>>,
}

impl<L: Layout> SmallArcBytes<L> {
    #[inline]
    pub fn new<B: Buffer<u8>>(buffer: B) -> Self {
        if buffer.is_array() {
            if let Some(small) = SmallBytes::new(buffer.as_slice()) {
                return Self(Inner { small });
            }
        }
        Self(Inner {
            arc: ManuallyDrop::new(ArcBytes::new(buffer)),
        })
    }

    #[inline]
    pub fn from_slice(slice: &[u8]) -> Self {
        if let Some(small) = SmallBytes::new(slice) {
            return Self(Inner { small });
        }
        Self(Inner {
            arc: ManuallyDrop::new(ArcBytes::new(slice.to_vec())),
        })
    }

    #[inline(always)]
    pub const fn as_either(&self) -> Either<&SmallBytes<L>, &ArcBytes<L>> {
        if unsafe { SmallBytes::is_inlined(addr_of!(self.0.small)) } {
            Either::Left(unsafe { &self.0.small })
        } else {
            Either::Right(unsafe { &*ptr_from_ref(&self.0.arc).cast() })
        }
    }

    #[inline(always)]
    pub fn as_either_mut(&mut self) -> Either<&mut SmallBytes<L>, &mut ArcBytes<L>> {
        if unsafe { SmallBytes::is_inlined(addr_of!(self.0.small)) } {
            Either::Left(unsafe { &mut self.0.small })
        } else {
            Either::Right(unsafe { &mut self.0.arc })
        }
    }

    #[inline(always)]
    pub fn into_either(self) -> Either<SmallBytes<L>, ArcBytes<L>> {
        let mut this = ManuallyDrop::new(self);
        if unsafe { SmallBytes::is_inlined(addr_of!(this.0.small)) } {
            Either::Left(unsafe { this.0.small })
        } else {
            Either::Right(unsafe { ManuallyDrop::take(&mut this.0.arc) })
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        match self.as_either() {
            Either::Left(bytes) => bytes.len(),
            Either::Right(bytes) => bytes.len(),
        }
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub const fn as_slice(&self) -> &[u8] {
        match self.as_either() {
            Either::Left(bytes) => bytes.as_slice(),
            Either::Right(bytes) => bytes.as_slice(),
        }
    }

    #[inline]
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self {
        match self.as_either() {
            Either::Left(bytes) => Self(Inner {
                small: bytes.subslice(range),
            }),
            Either::Right(bytes) => Self(Inner {
                arc: ManuallyDrop::new(bytes.subslice(range)),
            }),
        }
    }
}

impl<L: Layout> Drop for SmallArcBytes<L> {
    #[inline]
    fn drop(&mut self) {
        if let Either::Right(bytes) = self.as_either_mut() {
            unsafe { ptr::drop_in_place(bytes) }
        }
    }
}

impl<L: Layout> Clone for SmallArcBytes<L> {
    #[inline]
    fn clone(&self) -> Self {
        match self.as_either() {
            Either::Left(bytes) => Self(Inner { small: *bytes }),
            Either::Right(bytes) => Self(Inner {
                arc: ManuallyDrop::new(bytes.clone()),
            }),
        }
    }
}

impl<L: Layout> Deref for SmallArcBytes<L> {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<L: Layout> AsRef<[u8]> for SmallArcBytes<L> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl<L: Layout> Hash for SmallArcBytes<L> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<L: Layout> Borrow<[u8]> for SmallArcBytes<L> {
    #[inline]
    fn borrow(&self) -> &[u8] {
        self
    }
}

#[cfg(not(all(loom, test)))]
impl<L: Layout> Default for SmallArcBytes<L> {
    #[inline]
    fn default() -> Self {
        ArcBytes::new_static(&[]).into()
    }
}

impl<L: Layout> fmt::Debug for SmallArcBytes<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self, f)
    }
}

impl<L: Layout> fmt::LowerHex for SmallArcBytes<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.as_slice() {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl<L: Layout> fmt::UpperHex for SmallArcBytes<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.as_slice() {
            write!(f, "{:02X}", b)?;
        }
        Ok(())
    }
}

impl<L: Layout> PartialEq for SmallArcBytes<L> {
    fn eq(&self, other: &SmallArcBytes<L>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<L: Layout> Eq for SmallArcBytes<L> {}

impl<L: Layout> PartialOrd for SmallArcBytes<L> {
    fn partial_cmp(&self, other: &SmallArcBytes<L>) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<L: Layout> Ord for SmallArcBytes<L> {
    fn cmp(&self, other: &SmallArcBytes<L>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

macro_rules! std_impl {
    ($($(@$N:ident)? $ty:ty $(: $bound:path)?),*) => {$(
        impl<L: Layout, $(const $N: usize,)?> From<$ty> for SmallArcBytes<L> {

    #[inline]
            fn from(value: $ty) -> Self {
                Self::new(value)
            }
        }
    )*};
}
std_impl!(&'static [u8], @N &'static [u8; N], @N [u8; N], Box<[u8]>, Vec<u8>, Cow<'static, [u8]>: Clone);

impl<L: Layout> From<Either<SmallBytes<L>, ArcBytes<L>>> for SmallArcBytes<L> {
    #[inline]
    fn from(value: Either<SmallBytes<L>, ArcBytes<L>>) -> Self {
        match value {
            Either::Left(bytes) => Self(Inner { small: bytes }),
            Either::Right(bytes) => Self(Inner {
                arc: ManuallyDrop::new(bytes),
            }),
        }
    }
}

impl<L: Layout> From<SmallBytes<L>> for SmallArcBytes<L> {
    #[inline]
    fn from(value: SmallBytes<L>) -> Self {
        Either::<_, ArcBytes<L>>::Left(value).into()
    }
}

impl<L: Layout> From<ArcBytes<L>> for SmallArcBytes<L> {
    #[inline]
    fn from(value: ArcBytes<L>) -> Self {
        Either::<SmallBytes<L>, _>::Right(value).into()
    }
}

#[repr(transparent)]
pub struct SmallStr<L: Layout = Compact>(SmallBytes<L>);

impl<L: Layout> SmallStr<L> {
    #[inline]
    pub fn new(s: &str) -> Option<Self> {
        SmallBytes::new(s.as_bytes()).map(Self)
    }

    /// # Safety
    ///
    /// Bytes must be valid UTF-8.
    #[inline]
    pub const unsafe fn from_utf8_unchecked(bytes: SmallBytes<L>) -> Self {
        Self(bytes)
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
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
        Self(self.0.subslice(offset..offset + len))
    }

    #[inline]
    pub fn as_slice(&self) -> &SmallBytes<L> {
        &self.0
    }

    #[inline]
    pub fn into_slice(self) -> SmallBytes<L> {
        self.0
    }
}

impl<L: Layout> Clone for SmallStr<L> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Layout> Copy for SmallStr<L> {}

impl<L: Layout> Deref for SmallStr<L> {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl<L: Layout> AsRef<str> for SmallStr<L> {
    #[inline]
    fn as_ref(&self) -> &str {
        self
    }
}

impl<L: Layout> AsRef<[u8]> for SmallStr<L> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<L: Layout> Hash for SmallStr<L> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_bytes().hash(state);
    }
}

impl<L: Layout> Borrow<str> for SmallStr<L> {
    #[inline]
    fn borrow(&self) -> &str {
        self
    }
}

impl<L: Layout> Default for SmallStr<L> {
    #[inline]
    fn default() -> Self {
        Self::new("").unwrap()
    }
}

impl<L: Layout> fmt::Debug for SmallStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<L: Layout> fmt::Display for SmallStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<L: Layout> PartialEq for SmallStr<L> {
    fn eq(&self, other: &SmallStr<L>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<L: Layout> Eq for SmallStr<L> {}

impl<L: Layout> PartialOrd for SmallStr<L> {
    fn partial_cmp(&self, other: &SmallStr<L>) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<L: Layout> Ord for SmallStr<L> {
    fn cmp(&self, other: &SmallStr<L>) -> cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

pub struct SmallArcStr<L: Layout = Compact>(SmallArcBytes<L>);

impl<L: Layout> SmallArcStr<L> {
    #[inline]
    pub fn new<B: StringBuffer>(buffer: B) -> Self {
        unsafe { Self::from_utf8_unchecked(SmallArcBytes::new(StringBufWrapper(buffer))) }
    }

    #[inline]
    pub fn from_utf8(bytes: SmallArcBytes<L>) -> Result<Self, FromUtf8Error<SmallArcBytes<L>>> {
        match core::str::from_utf8(bytes.as_slice()) {
            Ok(_) => Ok(Self(bytes)),
            Err(error) => Err(FromUtf8Error { bytes, error }),
        }
    }

    /// # Safety
    ///
    /// Bytes must be valid UTF-8.
    #[inline]
    pub const unsafe fn from_utf8_unchecked(bytes: SmallArcBytes<L>) -> Self {
        Self(bytes)
    }

    #[inline(always)]
    pub fn as_either(&self) -> Either<&SmallStr<L>, &ArcStr<L>> {
        match self.0.as_either() {
            Either::Left(bytes) => unsafe {
                Either::Left(mem::transmute::<&SmallBytes<L>, &SmallStr<L>>(bytes))
            },
            Either::Right(bytes) => unsafe {
                Either::Right(mem::transmute::<&ArcBytes<L>, &ArcStr<L>>(bytes))
            },
        }
    }

    #[inline(always)]
    pub fn as_either_mut(&mut self) -> Either<&mut SmallStr<L>, &mut ArcStr<L>> {
        match self.0.as_either_mut() {
            Either::Left(bytes) => unsafe {
                Either::Left(mem::transmute::<&mut SmallBytes<L>, &mut SmallStr<L>>(
                    bytes,
                ))
            },
            Either::Right(bytes) => unsafe {
                Either::Right(mem::transmute::<&mut ArcBytes<L>, &mut ArcStr<L>>(bytes))
            },
        }
    }

    #[inline(always)]
    pub fn into_either(self) -> Either<SmallStr<L>, ArcStr<L>> {
        match self.0.into_either() {
            Either::Left(bytes) => unsafe { Either::Left(SmallStr::from_utf8_unchecked(bytes)) },
            Either::Right(bytes) => unsafe { Either::Right(ArcStr::from_utf8_unchecked(bytes)) },
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub const fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(self.0.as_slice()) }
    }

    #[inline]
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self {
        Self(self.0.subslice(range))
    }
}

impl<L: Layout> Clone for SmallArcStr<L> {
    #[inline]
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<L: Layout> Deref for SmallArcStr<L> {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl<L: Layout> AsRef<str> for SmallArcStr<L> {
    #[inline]
    fn as_ref(&self) -> &str {
        self
    }
}

impl<L: Layout> AsRef<[u8]> for SmallArcStr<L> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<L: Layout> Hash for SmallArcStr<L> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_str().hash(state);
    }
}

impl<L: Layout> Borrow<str> for SmallArcStr<L> {
    #[inline]
    fn borrow(&self) -> &str {
        self
    }
}

#[cfg(not(all(loom, test)))]
impl<L: Layout> Default for SmallArcStr<L> {
    #[inline]
    fn default() -> Self {
        ArcStr::new_static("").into()
    }
}

impl<L: Layout> fmt::Debug for SmallArcStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<L: Layout> fmt::Display for SmallArcStr<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<L: Layout> PartialEq for SmallArcStr<L> {
    fn eq(&self, other: &SmallArcStr<L>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<L: Layout> Eq for SmallArcStr<L> {}

impl<L: Layout> PartialOrd for SmallArcStr<L> {
    fn partial_cmp(&self, other: &SmallArcStr<L>) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<L: Layout> Ord for SmallArcStr<L> {
    fn cmp(&self, other: &SmallArcStr<L>) -> cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

macro_rules! std_impl {
    ($($ty:ty),*) => {$(
        impl<L: Layout> From<$ty> for SmallArcStr<L> {

            #[inline]
            fn from(value: $ty) -> Self {
                Self::new(value)
            }
        }
    )*};
}
std_impl!(
    &'static str,
    Box<str>,
    alloc::string::String,
    Cow<'static, str>
);

impl<L: Layout> FromStr for SmallArcStr<L> {
    type Err = Infallible;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(SmallArcBytes::from_slice(s.as_bytes())))
    }
}

impl<L: Layout> From<Either<SmallStr<L>, ArcStr<L>>> for SmallArcStr<L> {
    #[inline]
    fn from(value: Either<SmallStr<L>, ArcStr<L>>) -> Self {
        Self(match value {
            Either::Left(bytes) => bytes.into_slice().into(),
            Either::Right(bytes) => bytes.into_slice().into(),
        })
    }
}

impl<L: Layout> From<SmallStr<L>> for SmallArcStr<L> {
    #[inline]
    fn from(value: SmallStr<L>) -> Self {
        Either::<_, ArcStr<L>>::Left(value).into()
    }
}

impl<L: Layout> From<ArcStr<L>> for SmallArcStr<L> {
    #[inline]
    fn from(value: ArcStr<L>) -> Self {
        Either::<SmallStr<L>, _>::Right(value).into()
    }
}
