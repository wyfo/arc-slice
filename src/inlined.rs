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
pub(crate) use private::InlinedLayout;

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

mod private {
    #[allow(clippy::missing_safety_doc)]
    pub unsafe trait InlinedLayout {
        const LEN: usize;
        type Data: Copy;
        const UNINIT: Self::Data;
    }
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

/// An inlined storage that can contains a slice up to `size_of::<ArcBytes<L>>() - 2` bytes.
///
/// # Examples
///
/// ```rust
/// use arc_slice::inlined::SmallSlice;
///
/// let s = SmallSlice::<str>::new("hello world").unwrap();
/// assert_eq!(s, "hello world");
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

    /// An empty SmallSlice.
    pub const EMPTY: Self = Self {
        data: L::UNINIT,
        offset: 0,
        tagged_length: INLINED_FLAG,
        _phantom: PhantomData,
    };

    /// Create a new `SmallSlice` if the slice fits in.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallSlice;
    ///
    /// assert!(SmallSlice::<[u8]>::new(&[0, 1, 2]).is_some());
    /// assert!(SmallSlice::<[u8]>::new(&[0; 256]).is_none());
    /// ```
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

    /// Returns the number of items in the slice.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallSlice;
    ///
    /// let s = SmallSlice::<[u8]>::new(&[0, 1, 2]).unwrap();
    /// assert_eq!(s.len(), 3);
    /// ```
    pub const fn len(&self) -> usize {
        (self.tagged_length & !INLINED_FLAG) as usize
    }

    /// Returns `true` if the slice contains no items.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallSlice;
    ///
    /// let s = SmallSlice::<[u8]>::new(&[0, 1, 2]).unwrap();
    /// assert!(!s.is_empty());
    ///
    /// let s = SmallSlice::<[u8]>::new(&[]).unwrap();
    /// assert!(s.is_empty());
    /// ```
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a raw pointer to the slice's first item.
    ///
    /// See [`slice::as_ptr`].
    pub const fn as_ptr(&self) -> *const u8 {
        let data = ptr::from_ref(&self.data).cast::<u8>();
        unsafe { data.add(self.offset as usize) }
    }

    /// Advances the start of the slice by `offset` items.
    ///
    /// # Panics
    ///
    /// Panics if `offset > self.len()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallSlice;
    ///
    /// let mut s = SmallSlice::<[u8]>::new(b"hello world").unwrap();
    /// s.advance(6);
    /// assert_eq!(s, b"world");
    /// ```
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

    /// Truncate the slice to the first `len` items.
    ///
    /// If `len` is greater than the slice length, this has no effect.
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallSlice;
    ///
    /// let mut s = SmallSlice::<[u8]>::new(b"hello world").unwrap();
    /// s.truncate(5);
    /// assert_eq!(s, b"hello");
    /// ```
    pub fn truncate(&mut self, len: usize)
    where
        S: Subsliceable,
    {
        if len < self.len() {
            unsafe { self.check_truncate(len) };
            self.tagged_length = len as u8 | INLINED_FLAG;
        }
    }

    /// Extracts a subslice of a `SmallSlice` with a given range.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallSlice;
    ///
    /// let s = SmallSlice::<[u8]>::new(b"hello world").unwrap();
    /// let s2 = s.subslice(..5);
    /// assert_eq!(s2, b"hello");
    /// ```
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

/// A wrapper enabling [small string optimization] into [`ArcSlice`].
///
/// It can store up to `size_of::<ArcBytes<L>>() - 2` bytes inline, without allocating.
/// However, the niche optimization of `ArcSlice` is lost, which means that
/// `size_of::<Option<SmallArcBytes<L>>>() == size_of::<SmallArcBytes<L>>() + size_of::<usize>()`.
///
/// [small string optimization]: https://cppdepend.com/blog/understanding-small-string-optimization-sso-in-stdstring/
pub struct SmallArcSlice<S: Slice<Item = u8> + ?Sized, L: Layout = DefaultLayout>(Inner<S, L>);

#[repr(C)]
union Inner<S: Slice<Item = u8> + ?Sized, L: Layout> {
    small: SmallSlice<S, L>,
    arc: ManuallyDrop<ArcSlice<S, L>>,
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> SmallArcSlice<S, L> {
    /// Creates a new empty `SmallArcSlice`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// let s = SmallArcSlice::<[u8]>::new();
    /// assert_eq!(s, []);
    /// ```
    pub const fn new() -> Self {
        Self(Inner {
            small: SmallSlice::EMPTY,
        })
    }

    /// Creates a new `SmallArcSlice` by copying the given slice.
    ///
    /// The slice will be stored inlined if it can fit into a `SmallSlice`.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// let s = SmallArcSlice::<[u8]>::from_slice(b"hello world");
    /// assert_eq!(s, b"hello world");
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_slice(slice: &S) -> Self {
        SmallSlice::new(slice).map_or_else(|| ArcSlice::from_slice(slice).into(), Into::into)
    }

    /// Tries creating a new `SmallArcSlice` by copying the given slice, returning an error if the
    /// allocation fails.
    ///
    /// The slice will be stored inlined if it can fit into a `SmallSlice`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = SmallArcSlice::<[u8]>::try_from_slice(b"hello world")?;
    /// assert_eq!(s, b"hello world");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_from_slice(slice: &S) -> Result<Self, AllocError> {
        SmallSlice::new(slice).map_or_else(
            || Ok(ArcSlice::try_from_slice(slice)?.into()),
            |s| Ok(s.into()),
        )
    }

    /// Returns either a reference to the inlined [`SmallSlice`] storage, or to the [`ArcSlice`]
    /// one.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    /// use either::Either;
    ///
    /// let s = SmallArcSlice::<[u8]>::new();
    /// assert!(matches!(s.as_either(), Either::Left(_)));
    ///
    /// let s = SmallArcSlice::<[u8]>::from_array([0; 256]);
    /// assert!(matches!(s.as_either(), Either::Right(_)));
    /// ```
    #[inline(always)]
    pub fn as_either(&self) -> Either<&SmallSlice<S, L>, &ArcSlice<S, L>> {
        if unsafe { SmallSlice::is_inlined(addr_of!(self.0.small)) } {
            Either::Left(unsafe { &self.0.small })
        } else {
            Either::Right(unsafe { &*ptr::from_ref(&self.0.arc).cast() })
        }
    }

    /// Returns either a mutable reference to the inlined [`SmallSlice`] storage, or to the
    /// [`ArcSlice`] one.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    /// use either::Either;
    ///
    /// let mut s = SmallArcSlice::<[u8]>::new();
    /// assert!(matches!(s.as_either_mut(), Either::Left(_)));
    ///
    /// let mut s = SmallArcSlice::<[u8]>::from_array([0; 256]);
    /// assert!(matches!(s.as_either_mut(), Either::Right(_)));
    /// ```
    #[inline(always)]
    pub fn as_either_mut(&mut self) -> Either<&mut SmallSlice<S, L>, &mut ArcSlice<S, L>> {
        if unsafe { SmallSlice::is_inlined(addr_of!(self.0.small)) } {
            Either::Left(unsafe { &mut self.0.small })
        } else {
            Either::Right(unsafe { &mut self.0.arc })
        }
    }

    /// Returns either the inlined [`SmallSlice`] storage, or the [`ArcSlice`] one.
    #[inline(always)]
    pub fn into_either(self) -> Either<SmallSlice<S, L>, ArcSlice<S, L>> {
        let mut this = ManuallyDrop::new(self);
        if unsafe { SmallSlice::is_inlined(addr_of!(this.0.small)) } {
            Either::Left(unsafe { this.0.small })
        } else {
            Either::Right(unsafe { ManuallyDrop::take(&mut this.0.arc) })
        }
    }

    /// Returns the number of items in the slice.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// let s = SmallArcSlice::<[u8]>::from(&[0, 1, 2]);
    /// assert_eq!(s.len(), 3);
    /// ```
    pub fn len(&self) -> usize {
        match self.as_either() {
            Either::Left(bytes) => bytes.len(),
            Either::Right(bytes) => bytes.len(),
        }
    }

    /// Returns `true` if the slice contains no items.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// let s = SmallArcSlice::<[u8]>::from(&[0, 1, 2]);
    /// assert!(!s.is_empty());
    ///
    /// let s = SmallArcSlice::<[u8]>::from(&[]);
    /// assert!(s.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a raw pointer to the slice's first item.
    ///
    /// See [`slice::as_ptr`].
    pub fn as_ptr(&self) -> *const u8 {
        match self.as_either() {
            Either::Left(bytes) => bytes.as_ptr(),
            Either::Right(bytes) => bytes.start.as_ptr(),
        }
    }

    /// Tries cloning the `SmallArcSlice`, returning an error if an allocation fails.
    ///
    /// The operation may allocate. See [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout)
    /// documentation for cases where it does not.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = SmallArcSlice::<[u8]>::try_from_slice(b"hello world")?;
    /// let s2 = s.try_clone()?;
    /// assert_eq!(s2, b"hello world");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_clone(&self) -> Result<Self, AllocError> {
        Ok(match self.as_either() {
            Either::Left(bytes) => Self(Inner { small: *bytes }),
            Either::Right(bytes) => Self(Inner {
                arc: ManuallyDrop::new(bytes.try_clone()?),
            }),
        })
    }

    /// Tries extracting a subslice of an `SmallArcSlice` with a given range, returning an error
    /// if an allocation fails.
    ///
    /// The operation may allocate. See [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout)
    /// documentation for cases where it does not.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = SmallArcSlice::<[u8]>::try_from_slice(b"hello world")?;
    /// let s2 = s.try_subslice(..5)?;
    /// assert_eq!(s2, b"hello");
    /// # Ok(())
    /// # }
    /// ```
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

impl<L: Layout> SmallArcSlice<[u8], L> {
    /// Creates a new `SmallArcSlice` by moving the given array.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// let s = SmallArcSlice::<[u8]>::from_array([0, 1, 2]);
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_array<const N: usize>(array: [u8; N]) -> Self {
        SmallSlice::new(array.as_slice())
            .map_or_else(|| ArcSlice::from_array(array).into(), Into::into)
    }

    /// Tries creating a new `SmallArcSlice` by moving the given array, returning it if an
    /// allocation fails.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// let s = SmallArcSlice::<[u8]>::try_from_array([0, 1, 2]).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    pub fn try_from_array<const N: usize>(array: [u8; N]) -> Result<Self, [u8; N]> {
        SmallSlice::new(array.as_slice()).map_or_else(
            || Ok(ArcSlice::try_from_array(array)?.into()),
            |a| Ok(a.into()),
        )
    }
}

impl<
        S: Slice<Item = u8> + ?Sized,
        #[cfg(feature = "oom-handling")] L: Layout,
        #[cfg(not(feature = "oom-handling"))] L: CloneNoAllocLayout,
    > SmallArcSlice<S, L>
{
    /// Extracts a subslice of an `SmallArcSlice` with a given range.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::inlined::SmallArcSlice;
    ///
    /// let s = SmallArcSlice::<[u8]>::from_slice(b"hello world");
    /// let s2 = s.subslice(..5);
    /// assert_eq!(s2, b"hello");
    /// ```
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
    /// Creates a new `SmallArcSlice` from a static slice.
    ///
    /// The operation never allocates.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{inlined::SmallArcSlice, layout::ArcLayout};
    ///
    /// static HELLO_WORLD: SmallArcSlice<[u8], ArcLayout<true, true>> =
    ///     SmallArcSlice::<[u8], ArcLayout<true, true>>::from_static(b"hello world");
    /// ```
    pub const fn from_static(slice: &'static [u8]) -> SmallArcSlice<[u8], L> {
        Self(Inner {
            arc: ManuallyDrop::new(ArcSlice::<[u8], L>::from_static(slice)),
        })
    }
}

impl<L: StaticLayout> SmallArcSlice<str, L> {
    /// Creates a new `SmallArcSlice` from a static slice.
    ///
    /// The operation never allocates.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{inlined::SmallArcSlice, layout::ArcLayout};
    ///
    /// static HELLO_WORLD: SmallArcSlice<[u8], ArcLayout<true, true>> =
    ///     SmallArcSlice::<[u8], ArcLayout<true, true>>::from_static(b"hello world");
    /// ```
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
impl<S: Slice<Item = u8> + ?Sized, L: AnyBufferLayout> From<&S> for SmallArcSlice<S, L> {
    fn from(value: &S) -> Self {
        Self::from_slice(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<L: AnyBufferLayout, const N: usize> From<&[u8; N]> for SmallArcSlice<[u8], L> {
    fn from(value: &[u8; N]) -> Self {
        Self::from_slice(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<L: AnyBufferLayout, const N: usize> From<[u8; N]> for SmallArcSlice<[u8], L> {
    fn from(value: [u8; N]) -> Self {
        Self::from_array(value)
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

/// An alias for `SmallArcSlice<[u8], L>`.
pub type SmallArcBytes<L = DefaultLayout> = SmallArcSlice<[u8], L>;
/// An alias for `SmallArcSlice<str, L>`.
pub type SmallArcStr<L = DefaultLayout> = SmallArcSlice<str, L>;
