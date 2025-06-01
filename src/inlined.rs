//! [Small String Optimization] support for [`ArcSlice`].
//!
//! [Small String Optimization]: https://cppdepend.com/blog/understanding-small-string-optimization-sso-in-stdstring/

use alloc::{string::String, vec::Vec};
use core::{
    borrow::Borrow,
    cmp, fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem::{size_of, ManuallyDrop, MaybeUninit},
    ops::{Deref, RangeBounds},
    ptr::addr_of,
    slice,
};

use either::Either;

#[cfg(feature = "oom-handling")]
use crate::layout::AnyBufferLayout;
#[cfg(not(feature = "oom-handling"))]
use crate::layout::CloneNoAllocLayout;
use crate::{
    buffer::{Emptyable, Slice, SliceExt, Subsliceable},
    error::AllocError,
    layout::{ArcLayout, BoxedSliceLayout, DefaultLayout, Layout, StaticLayout, VecLayout},
    msrv::ptr,
    utils::{debug_slice, lower_hex, panic_out_of_range, range_offset_len, upper_hex},
    ArcSlice,
};

const INLINED_FLAG: u8 = 0x80;

#[allow(clippy::missing_safety_doc)]
pub unsafe trait InlinedLayout {
    const LEN: usize;
    type Data: Copy;
    const UNINIT: Self::Data;
}

const _3_WORDS_LEN: usize = 3 * size_of::<usize>() - 2;
const _4_WORDS_LEN: usize = 4 * size_of::<usize>() - 2;

unsafe impl<const ANY_BUFFER: bool, const STATIC: bool> InlinedLayout
    for ArcLayout<ANY_BUFFER, STATIC>
{
    const LEN: usize = _3_WORDS_LEN;
    type Data = [MaybeUninit<u8>; _3_WORDS_LEN];
    const UNINIT: Self::Data = [MaybeUninit::uninit(); _3_WORDS_LEN];
}

unsafe impl InlinedLayout for BoxedSliceLayout {
    const LEN: usize = _3_WORDS_LEN;
    type Data = [MaybeUninit<u8>; _3_WORDS_LEN];
    const UNINIT: Self::Data = [MaybeUninit::uninit(); _3_WORDS_LEN];
}

unsafe impl InlinedLayout for VecLayout {
    const LEN: usize = _4_WORDS_LEN;
    type Data = [MaybeUninit<u8>; _4_WORDS_LEN];
    const UNINIT: Self::Data = [MaybeUninit::uninit(); _4_WORDS_LEN];
}

#[cfg(feature = "raw-buffer")]
unsafe impl InlinedLayout for crate::layout::RawLayout {
    const LEN: usize = _4_WORDS_LEN;
    type Data = [MaybeUninit<u8>; _4_WORDS_LEN];
    const UNINIT: Self::Data = [MaybeUninit::uninit(); _4_WORDS_LEN];
}

/// An inlined storage that can contains up to `size_of::<ArcBytes<L>>() - 2` bytes.
#[repr(C)]
pub struct SmallSlice<S: Slice<Item = u8> + ?Sized, L: Layout = DefaultLayout> {
    #[cfg(target_endian = "big")]
    tagged_length: u8,
    data: <L as InlinedLayout>::Data,
    offset: u8,
    #[cfg(target_endian = "little")]
    tagged_length: u8,
    _phantom: PhantomData<S>,
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> SmallSlice<S, L> {
    const MAX_LEN: usize = L::LEN;

    pub const EMPTY: Self = Self {
        data: L::UNINIT,
        offset: 0,
        tagged_length: INLINED_FLAG,
        _phantom: PhantomData,
    };

    /// Create a new [`SmallSlice`] if the slice fit in.
    pub fn new(slice: &S) -> Option<Self> {
        if slice.len() > Self::MAX_LEN {
            return None;
        }
        let mut this = Self {
            data: L::UNINIT,
            offset: 0,
            tagged_length: slice.len() as u8 | INLINED_FLAG,
            _phantom: PhantomData,
        };
        let data = ptr::from_mut(&mut this.data).cast::<u8>();
        unsafe { ptr::copy_nonoverlapping(slice.as_ptr().as_ptr(), data, slice.len()) }
        Some(this)
    }

    #[inline(always)]
    const fn is_inlined(this: *const Self) -> bool {
        unsafe { (*addr_of!((*this).tagged_length)) & INLINED_FLAG != 0 }
    }

    pub const fn len(&self) -> usize {
        (self.tagged_length & !INLINED_FLAG) as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn as_ptr(&self) -> *const u8 {
        let data = ptr::from_ref(&self.data).cast::<u8>();
        unsafe { data.add(self.offset as usize) }
    }

    pub fn advance(&mut self, offset: usize)
    where
        S: Subsliceable,
    {
        if offset > self.len() {
            panic_out_of_range()
        }
        unsafe { self.check_advance(offset) };
        self.offset += offset as u8;
        self.tagged_length -= offset as u8;
    }

    pub fn truncate(&mut self, len: usize)
    where
        S: Subsliceable,
    {
        if len < self.len() {
            unsafe { self.check_truncate(len) };
            self.tagged_length = len as u8 | INLINED_FLAG;
        }
    }

    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self
    where
        S: Subsliceable,
    {
        let (offset, len) = range_offset_len(self.deref(), range);
        Self {
            offset: self.offset + offset as u8,
            tagged_length: len as u8 | INLINED_FLAG,
            ..*self
        }
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> Clone for SmallSlice<S, L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> Copy for SmallSlice<S, L> {}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> Deref for SmallSlice<S, L> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        unsafe { S::from_slice_unchecked(slice::from_raw_parts(self.as_ptr(), self.len())) }
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> AsRef<S> for SmallSlice<S, L> {
    fn as_ref(&self) -> &S {
        self
    }
}

impl<S: Hash + Slice<Item = u8> + ?Sized, L: Layout> Hash for SmallSlice<S, L> {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.deref().hash(state);
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> Borrow<S> for SmallSlice<S, L> {
    fn borrow(&self) -> &S {
        self
    }
}

impl<S: Emptyable<Item = u8> + ?Sized, L: Layout> Default for SmallSlice<S, L> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<S: fmt::Debug + Slice<Item = u8> + ?Sized, L: Layout> fmt::Debug for SmallSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self.deref(), f)
    }
}

impl<S: fmt::Display + Slice<Item = u8> + ?Sized, L: Layout> fmt::Display for SmallSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::LowerHex for SmallSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self.to_slice(), f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::UpperHex for SmallSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self.to_slice(), f)
    }
}

impl<S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> PartialEq for SmallSlice<S, L> {
    fn eq(&self, other: &SmallSlice<S, L>) -> bool {
        **self == **other
    }
}

impl<S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> Eq for SmallSlice<S, L> {}

impl<S: PartialOrd + Slice<Item = u8> + ?Sized, L: Layout> PartialOrd for SmallSlice<S, L> {
    fn partial_cmp(&self, other: &SmallSlice<S, L>) -> Option<cmp::Ordering> {
        self.deref().partial_cmp(other.deref())
    }
}

impl<S: Ord + Slice<Item = u8> + ?Sized, L: Layout> Ord for SmallSlice<S, L> {
    fn cmp(&self, other: &SmallSlice<S, L>) -> cmp::Ordering {
        self.deref().cmp(other.deref())
    }
}

impl<S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> PartialEq<S> for SmallSlice<S, L> {
    fn eq(&self, other: &S) -> bool {
        self.deref() == other
    }
}

impl<'a, S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> PartialEq<&'a S>
    for SmallSlice<S, L>
{
    fn eq(&self, other: &&'a S) -> bool {
        self.deref() == *other
    }
}

impl<L: Layout, const N: usize> PartialEq<[u8; N]> for SmallSlice<[u8], L> {
    fn eq(&self, other: &[u8; N]) -> bool {
        *other == **self
    }
}

impl<'a, L: Layout, const N: usize> PartialEq<&'a [u8; N]> for SmallSlice<[u8], L> {
    fn eq(&self, other: &&'a [u8; N]) -> bool {
        **other == **self
    }
}

impl<L: Layout, const N: usize> PartialEq<SmallSlice<[u8], L>> for [u8; N] {
    fn eq(&self, other: &SmallSlice<[u8], L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<SmallSlice<[u8], L>> for [u8] {
    fn eq(&self, other: &SmallSlice<[u8], L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<SmallSlice<str, L>> for str {
    fn eq(&self, other: &SmallSlice<str, L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<Vec<u8>> for SmallSlice<[u8], L> {
    fn eq(&self, other: &Vec<u8>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<String> for SmallSlice<str, L> {
    fn eq(&self, other: &String) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<SmallSlice<[u8], L>> for Vec<u8> {
    fn eq(&self, other: &SmallSlice<[u8], L>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<SmallSlice<str, L>> for String {
    fn eq(&self, other: &SmallSlice<str, L>) -> bool {
        **self == **other
    }
}

/// As SSO-enabled implementation of [`ArcSlice`].
///
/// It can store up to `size_of::<ArcBytes<L>>() - 2` bytes without allocating. However,
/// the niche optimization of `ArcSlice` is lost, which means that
/// `size_of::<SmallArcSlice<[u8], L>>() == size_of::<ArcSlice<[u8], L>>() + size_of::<usize>()`.
pub struct SmallArcSlice<S: Slice<Item = u8> + ?Sized, L: Layout = DefaultLayout>(Inner<S, L>);

#[repr(C)]
union Inner<S: Slice<Item = u8> + ?Sized, L: Layout> {
    small: SmallSlice<S, L>,
    arc: ManuallyDrop<ArcSlice<S, L>>,
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> SmallArcSlice<S, L> {
    pub const fn new() -> Self {
        Self(Inner {
            small: SmallSlice::EMPTY,
        })
    }

    #[cfg(feature = "oom-handling")]
    pub fn from_slice(slice: &S) -> Self {
        SmallSlice::new(slice).map_or_else(|| ArcSlice::from_slice(slice).into(), Into::into)
    }

    pub fn try_from_slice(slice: &S) -> Result<Self, AllocError> {
        SmallSlice::new(slice).map_or_else(
            || Ok(ArcSlice::try_from_slice(slice)?.into()),
            |s| Ok(s.into()),
        )
    }

    #[inline(always)]
    pub fn as_either(&self) -> Either<&SmallSlice<S, L>, &ArcSlice<S, L>> {
        if unsafe { SmallSlice::is_inlined(addr_of!(self.0.small)) } {
            Either::Left(unsafe { &self.0.small })
        } else {
            Either::Right(unsafe { &*ptr::from_ref(&self.0.arc).cast() })
        }
    }

    #[inline(always)]
    pub fn as_either_mut(&mut self) -> Either<&mut SmallSlice<S, L>, &mut ArcSlice<S, L>> {
        if unsafe { SmallSlice::is_inlined(addr_of!(self.0.small)) } {
            Either::Left(unsafe { &mut self.0.small })
        } else {
            Either::Right(unsafe { &mut self.0.arc })
        }
    }

    #[inline(always)]
    pub fn into_either(self) -> Either<SmallSlice<S, L>, ArcSlice<S, L>> {
        let mut this = ManuallyDrop::new(self);
        if unsafe { SmallSlice::is_inlined(addr_of!(this.0.small)) } {
            Either::Left(unsafe { this.0.small })
        } else {
            Either::Right(unsafe { ManuallyDrop::take(&mut this.0.arc) })
        }
    }

    pub fn len(&self) -> usize {
        match self.as_either() {
            Either::Left(bytes) => bytes.len(),
            Either::Right(bytes) => bytes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_ptr(&self) -> *const u8 {
        match self.as_either() {
            Either::Left(bytes) => bytes.as_ptr(),
            Either::Right(bytes) => bytes.start.as_ptr(),
        }
    }

    pub fn try_subslice(&self, range: impl RangeBounds<usize>) -> Result<Self, AllocError>
    where
        S: Subsliceable,
    {
        match self.as_either() {
            Either::Left(bytes) => Ok(bytes.subslice(range).into()),
            Either::Right(bytes) => Ok(bytes.try_subslice(range)?.into()),
        }
    }

    #[doc(hidden)]
    pub fn _advance(&mut self, cnt: usize)
    where
        S: Subsliceable,
    {
        match self.as_either_mut() {
            Either::Left(s) => s.advance(cnt),
            Either::Right(s) => s.advance(cnt),
        }
    }
}

impl<
        S: Slice<Item = u8> + ?Sized,
        #[cfg(feature = "oom-handling")] L: Layout,
        #[cfg(not(feature = "oom-handling"))] L: CloneNoAllocLayout,
    > SmallArcSlice<S, L>
{
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self
    where
        S: Subsliceable,
    {
        match self.as_either() {
            Either::Left(bytes) => bytes.subslice(range).into(),
            Either::Right(bytes) => bytes.subslice(range).into(),
        }
    }
}

impl<L: StaticLayout> SmallArcSlice<[u8], L> {
    pub const fn from_static(slice: &'static [u8]) -> SmallArcSlice<[u8], L> {
        Self(Inner {
            arc: ManuallyDrop::new(ArcSlice::<[u8], L>::from_static(slice)),
        })
    }
}

impl<L: StaticLayout> SmallArcSlice<str, L> {
    pub const fn from_static(slice: &'static str) -> SmallArcSlice<str, L> {
        Self(Inner {
            arc: ManuallyDrop::new(ArcSlice::<str, L>::from_static(slice)),
        })
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> Drop for SmallArcSlice<S, L> {
    fn drop(&mut self) {
        if let Either::Right(bytes) = self.as_either_mut() {
            unsafe { ptr::drop_in_place(bytes) }
        }
    }
}

impl<
        S: Slice<Item = u8> + ?Sized,
        #[cfg(feature = "oom-handling")] L: Layout,
        #[cfg(not(feature = "oom-handling"))] L: CloneNoAllocLayout,
    > Clone for SmallArcSlice<S, L>
{
    fn clone(&self) -> Self {
        match self.as_either() {
            Either::Left(bytes) => Self(Inner { small: *bytes }),
            Either::Right(bytes) => Self(Inner {
                arc: ManuallyDrop::new(bytes.clone()),
            }),
        }
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> Deref for SmallArcSlice<S, L> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        match self.as_either() {
            Either::Left(bytes) => bytes,
            Either::Right(bytes) => bytes,
        }
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> AsRef<S> for SmallArcSlice<S, L> {
    fn as_ref(&self) -> &S {
        self
    }
}

impl<S: Hash + Slice<Item = u8> + ?Sized, L: Layout> Hash for SmallArcSlice<S, L> {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.deref().hash(state);
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> Borrow<S> for SmallArcSlice<S, L> {
    fn borrow(&self) -> &S {
        self
    }
}

impl<S: Emptyable<Item = u8> + ?Sized, L: Layout> Default for SmallArcSlice<S, L> {
    fn default() -> Self {
        Self::from(SmallSlice::default())
    }
}

impl<S: fmt::Debug + Slice<Item = u8> + ?Sized, L: Layout> fmt::Debug for SmallArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self.deref(), f)
    }
}

impl<S: fmt::Display + Slice<Item = u8> + ?Sized, L: Layout> fmt::Display for SmallArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::LowerHex for SmallArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self.to_slice(), f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::UpperHex for SmallArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self.to_slice(), f)
    }
}

impl<S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> PartialEq for SmallArcSlice<S, L> {
    fn eq(&self, other: &SmallArcSlice<S, L>) -> bool {
        **self == **other
    }
}

impl<S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> Eq for SmallArcSlice<S, L> {}

impl<S: PartialOrd + Slice<Item = u8> + ?Sized, L: Layout> PartialOrd for SmallArcSlice<S, L> {
    fn partial_cmp(&self, other: &SmallArcSlice<S, L>) -> Option<cmp::Ordering> {
        self.deref().partial_cmp(other.deref())
    }
}

impl<S: Ord + Slice<Item = u8> + ?Sized, L: Layout> Ord for SmallArcSlice<S, L> {
    fn cmp(&self, other: &SmallArcSlice<S, L>) -> cmp::Ordering {
        self.deref().cmp(other.deref())
    }
}

impl<S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> PartialEq<S> for SmallArcSlice<S, L> {
    fn eq(&self, other: &S) -> bool {
        self.deref() == other
    }
}

impl<'a, S: PartialEq + Slice<Item = u8> + ?Sized, L: Layout> PartialEq<&'a S>
    for SmallArcSlice<S, L>
{
    fn eq(&self, other: &&'a S) -> bool {
        self.deref() == *other
    }
}

impl<L: Layout, const N: usize> PartialEq<[u8; N]> for SmallArcSlice<[u8], L> {
    fn eq(&self, other: &[u8; N]) -> bool {
        *other == **self
    }
}

impl<'a, L: Layout, const N: usize> PartialEq<&'a [u8; N]> for SmallArcSlice<[u8], L> {
    fn eq(&self, other: &&'a [u8; N]) -> bool {
        **other == **self
    }
}

impl<L: Layout, const N: usize> PartialEq<SmallArcSlice<[u8], L>> for [u8; N] {
    fn eq(&self, other: &SmallArcSlice<[u8], L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<SmallArcSlice<[u8], L>> for [u8] {
    fn eq(&self, other: &SmallArcSlice<[u8], L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<SmallArcSlice<str, L>> for str {
    fn eq(&self, other: &SmallArcSlice<str, L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<Vec<u8>> for SmallArcSlice<[u8], L> {
    fn eq(&self, other: &Vec<u8>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<String> for SmallArcSlice<str, L> {
    fn eq(&self, other: &String) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<SmallArcSlice<[u8], L>> for Vec<u8> {
    fn eq(&self, other: &SmallArcSlice<[u8], L>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<SmallArcSlice<str, L>> for String {
    fn eq(&self, other: &SmallArcSlice<str, L>) -> bool {
        **self == **other
    }
}

#[cfg(feature = "oom-handling")]
impl<S: Slice<Item = u8> + ?Sized, L: AnyBufferLayout> From<alloc::boxed::Box<S>>
    for SmallArcSlice<S, L>
{
    fn from(value: alloc::boxed::Box<S>) -> Self {
        ArcSlice::from(value).into()
    }
}

#[cfg(feature = "oom-handling")]
impl<L: AnyBufferLayout> From<Vec<u8>> for SmallArcSlice<[u8], L> {
    fn from(value: Vec<u8>) -> Self {
        ArcSlice::from(value).into()
    }
}

#[cfg(feature = "oom-handling")]
impl<L: AnyBufferLayout> From<String> for SmallArcSlice<str, L> {
    fn from(value: String) -> Self {
        ArcSlice::from(value).into()
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> From<SmallSlice<S, L>> for SmallArcSlice<S, L> {
    fn from(value: SmallSlice<S, L>) -> Self {
        Self(Inner { small: value })
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> From<ArcSlice<S, L>> for SmallArcSlice<S, L> {
    fn from(value: ArcSlice<S, L>) -> Self {
        Self(Inner {
            arc: ManuallyDrop::new(value),
        })
    }
}

#[cfg(feature = "oom-handling")]
impl<L: Layout> core::str::FromStr for SmallArcSlice<str, L> {
    type Err = core::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_slice(s))
    }
}
