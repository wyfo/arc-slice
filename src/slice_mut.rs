use alloc::{string::String, vec::Vec};
use core::{
    any::Any,
    borrow::{Borrow, BorrowMut},
    cmp,
    convert::Infallible,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    slice,
};

#[cfg(not(feature = "oom-handling"))]
use crate::layout::{ArcLayout, CloneNoAllocLayout, VecLayout};
#[allow(unused_imports)]
use crate::msrv::{NonNullExt, OptionExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{
        BorrowMetadata, BufferExt, BufferMut, BufferWithMetadata, Concatenable, DynBuffer,
        Emptyable, Extendable, Slice, SliceExt, Zeroable,
    },
    error::{AllocError, AllocErrorImpl, TryReserveError},
    layout::{AnyBufferLayout, DefaultLayoutMut, FromLayout, Layout, LayoutMut},
    macros::{assume, is},
    msrv::ptr,
    slice::ArcSliceLayout,
    utils::{
        debug_slice, lower_hex, min_non_zero_cap, panic_out_of_range, transmute_checked,
        try_transmute, upper_hex, UnwrapChecked, UnwrapInfallible,
    },
    ArcSlice,
};
#[cfg(feature = "serde")]
use crate::{buffer::Buffer, utils::assert_checked};

mod arc;
mod vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Data<const UNIQUE: bool>(pub(crate) NonNull<()>);

impl<S: Slice + ?Sized, const ANY_BUFFER: bool> From<Arc<S, ANY_BUFFER>> for Data<true> {
    fn from(value: Arc<S, ANY_BUFFER>) -> Self {
        Self(value.into_raw())
    }
}

pub(crate) type TryReserveResult<T> = (Result<usize, TryReserveError>, NonNull<T>);

#[allow(clippy::missing_safety_doc)]
pub unsafe trait ArcSliceMutLayout {
    const ANY_BUFFER: bool;
    fn try_data_from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool, const UNIQUE: bool>(
        arc: ManuallyDrop<Arc<S, ANY_BUFFER>>,
    ) -> Option<Data<UNIQUE>>;
    unsafe fn data_from_vec<S: Slice + ?Sized, E: AllocErrorImpl, const UNIQUE: bool>(
        vec: S::Vec,
        offset: usize,
    ) -> Result<Data<UNIQUE>, (E, S::Vec)>;
    fn clone<S: Slice + ?Sized, E: AllocErrorImpl, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data<UNIQUE>,
    ) -> Result<(), E>;
    unsafe fn drop<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    );
    fn advance<S: Slice + ?Sized, const UNIQUE: bool>(
        _data: Option<&mut Data<UNIQUE>>,
        _offset: usize,
    ) {
    }
    fn truncate<S: Slice + ?Sized, const UNIQUE: bool>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        _data: &mut Data<UNIQUE>,
    ) {
    }
    fn get_metadata<S: Slice + ?Sized, M: Any, const UNIQUE: bool>(
        data: &Data<UNIQUE>,
    ) -> Option<&M>;
    unsafe fn take_buffer<S: Slice + ?Sized, B: BufferMut<S>, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<B>;
    unsafe fn take_array<T: Send + Sync + 'static, const N: usize, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        data: Data<UNIQUE>,
    ) -> Option<[T; N]>;
    fn is_unique<S: Slice + ?Sized, const UNIQUE: bool>(data: &mut Data<UNIQUE>) -> bool;
    fn try_reserve<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data<UNIQUE>,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<S::Item>;
    fn frozen_data<S: Slice + ?Sized, L: ArcSliceLayout, E: AllocErrorImpl, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<L::Data>;
    fn update_layout<
        S: Slice + ?Sized,
        L: ArcSliceMutLayout,
        E: AllocErrorImpl,
        const UNIQUE: bool,
    >(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data<UNIQUE>,
    ) -> Option<Data<UNIQUE>>;
}

/// A thread-safe, mutable and growable container.
///
/// `ArcSliceMut` has a smaller choice of [layout] than [`ArcSlice`], but can also wrap arbitrary
/// buffers such as`Vec`, memory-mapped files, etc. Arbitrary metadata can also be attached to the
/// buffer for contextual or domain-specific needs.
///
/// With `UNIQUE=true`, `ArcSliceMut` is roughly equivalent to a `Vec`. Additional capacity
/// can be reserved and the slice can be extended. With `UNIQUE=false`, the slice may be shared,
/// meaning that multiple `ArcSliceMut` may reference non-intersecting portions of the underlying
/// buffer. In that case, capacity reservation might fail, but the slice will never be implicitly
/// reallocated and copied. In any case, `ArcSliceMut` is cheaply convertible to [`ArcSlice`].
///
/// It is mainly intended to manipulate `[u8]`/`str` byte slices, to facilitate zero-copy
/// operations in network programming, hence the aliases [`ArcBytesMut`]/[`ArcStrMut`]. But it can
/// actually handle any type of slices, from strings with specific invariants to primitive slices
/// with droppable items.
///
/// # Examples
///
/// ```rust
/// use arc_slice::{ArcSlice, ArcSliceMut};
///
/// let mut s = ArcSliceMut::<[u8]>::with_capacity(64);
/// s.push(b'h');
/// s.extend_from_slice(b"ello");
/// assert_eq!(s, b"hello");
///
/// let mut s = s.into_shared();
/// let mut s2 = s.split_off(3);
/// s.copy_from_slice(b"bye");
/// s2.copy_from_slice(b"!!");
/// s.try_unsplit(s2).unwrap();
/// assert_eq!(s, b"bye!!");
///
/// let frozen: ArcSlice<[u8]> = s.freeze();
/// assert_eq!(frozen, b"bye!!");
/// ```
///
/// With shared memory:
/// ```rust
/// use std::{
///     fs::File,
///     path::{Path, PathBuf},
/// };
///
/// use arc_slice::{buffer::AsMutBuffer, error::TryReserveError, layout::ArcLayout, ArcSliceMut};
/// use memmap2::MmapMut;
///
/// # fn main() -> std::io::Result<()> {
/// let path = Path::new("README.md").to_owned();
/// # #[cfg(not(miri))]
/// let file = File::options().read(true).write(true).open(&path)?;
/// # #[cfg(not(miri))]
/// let mmap = unsafe { MmapMut::map_mut(&file)? };
/// # #[cfg(miri)]
/// # let mmap = b"# arc-slice".to_vec();
///
/// let buffer = unsafe { AsMutBuffer::new(mmap) };
/// let mut bytes: ArcSliceMut<[u8], ArcLayout<true>> =
///     ArcSliceMut::from_buffer_with_metadata(buffer, path);
/// bytes[..11].copy_from_slice(b"# arc-slice");
/// assert!(bytes.starts_with(b"# arc-slice"));
/// assert_eq!(bytes.metadata::<PathBuf>().unwrap(), Path::new("README.md"));
/// # Ok(())
/// # }
/// ```
///
/// [layout]: crate::layout
/// [`ArcBytesMut`]: crate::ArcBytesMut
/// [`ArcStrMut`]: crate::ArcStrMut
pub struct ArcSliceMut<
    S: Slice + ?Sized,
    L: LayoutMut = DefaultLayoutMut,
    const UNIQUE: bool = true,
> {
    start: NonNull<S::Item>,
    length: usize,
    capacity: usize,
    data: Option<Data<UNIQUE>>,
    _phantom: PhantomData<L>,
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> ArcSliceMut<S, L, UNIQUE> {
    /// Returns the number of items in the slice.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::from(&[0, 1, 2]);
    /// assert_eq!(s.len(), 3);
    /// ```
    pub const fn len(&self) -> usize {
        self.length
    }

    /// Returns `true` if the slice contains no items.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::from(&[0, 1, 2]);
    /// assert!(!s.is_empty());
    ///
    /// let s = ArcSliceMut::<[u8]>::from(&[]);
    /// assert!(s.is_empty());
    /// ```
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a raw pointer to the slice's first item.
    ///
    /// See [`slice::as_ptr`].
    pub const fn as_ptr(&self) -> *const S::Item {
        self.start.as_ptr()
    }

    /// Returns a mutable raw pointer to the slice's first item.
    ///
    /// See [`slice::as_mut_ptr`].
    pub fn as_mut_ptr(&mut self) -> *mut S::Item {
        self.start.as_ptr()
    }

    /// Returns a reference to the underlying slice.
    ///
    /// Equivalent to `&self[..]`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::from(b"hello world");
    /// assert_eq!(s.as_slice(), b"hello world");
    /// ```
    pub fn as_slice(&self) -> &S {
        unsafe { S::from_raw_parts(self.start, self.len()) }
    }

    /// Returns a mutable reference to the underlying slice.
    ///
    /// Equivalent to `&mut self[..]`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::from(b"hello world");
    /// assert_eq!(s.as_mut_slice(), b"hello world");
    /// ```
    pub fn as_mut_slice(&mut self) -> &mut S {
        unsafe { S::from_raw_parts_mut(self.start, self.len()) }
    }

    /// Returns the total number of items the slice can hold without reallocating.
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::with_capacity(64);
    /// s.push(0);
    /// assert_eq!(s.capacity(), 64);
    /// ```
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    fn spare_capacity(&self) -> usize {
        self.capacity - self.length
    }

    /// Returns the remaining spare capacity of the slice.
    ///
    /// The returned slice can be used to fill the slice with items before marking the data as
    /// initialized using the [`set_len`](Self::set_len) method.
    ///
    /// # Safety
    ///
    /// Writing uninitialized memory may be unsound if the underlying buffer doesn't support it.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    /// let mut s = ArcSliceMut::<[u8]>::with_capacity(10);
    ///
    /// // SAFETY: no uninit bytes are written
    /// let uninit = unsafe { s.spare_capacity_mut() };
    /// uninit[0].write(0);
    /// uninit[1].write(1);
    /// uninit[2].write(2);
    /// // SAFETY: the first 3 bytes are initialized
    /// unsafe { s.set_len(3) }
    ///
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    pub unsafe fn spare_capacity_mut(&mut self) -> &mut [MaybeUninit<S::Item>]
    where
        S: Extendable,
    {
        unsafe {
            let end = self.start.as_ptr().add(self.length).cast();
            slice::from_raw_parts_mut(end, self.spare_capacity())
        }
    }

    /// Forces the length of the slice to `new_len`.
    ///
    /// # Safety
    ///
    /// First `len` items of the slice must be initialized.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    /// let mut s = ArcSliceMut::<[u8]>::with_capacity(10);
    ///
    /// // SAFETY: `s.capacity()` >= 3
    /// unsafe { std::ptr::copy_nonoverlapping([0, 1, 2].as_ptr(), s.as_mut_ptr(), 3) };
    ///
    /// // SAFETY: the first 3 bytes are initialized
    /// unsafe { s.set_len(3) }
    ///
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    pub unsafe fn set_len(&mut self, new_len: usize)
    where
        S: Extendable,
    {
        self.length = new_len;
    }

    /// Tries appending an element to the end of the slice, returning an error if the capacity
    /// reservation fails.
    ///
    /// The buffer might have to reserve additional capacity to do the appending.
    ///
    /// The default arc-slice buffer supports amortized reservation, doubling the capacity each
    /// time.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// # fn main() -> Result<(), arc_slice::error::TryReserveError> {
    /// let mut s = ArcSliceMut::<[u8]>::new();
    /// s.try_push(42)?;
    /// assert_eq!(s, [42]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_push(&mut self, item: S::Item) -> Result<(), TryReserveError>
    where
        S: Extendable,
    {
        self.try_reserve(1)?;
        unsafe { self.start.as_ptr().add(self.length).write(item) };
        self.length += 1;
        Ok(())
    }

    /// Tries reclaiming additional capacity for at least `additional` more items without
    /// reallocating the buffer, returning `true` if it succeeds.
    ///
    /// Does nothing if the spare capacity is greater than the requested one.
    ///
    /// Reclaiming means shifting the current slice to the front of the buffer. It is only possible
    /// when the `ArcSliceMut` is unique, and when the slice doesn't overlap with the spare
    /// capacity at the buffer front.
    ///
    /// The reclaimed capacity might be greater than the requested one.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::from_iter(0..64);
    /// let ptr = s.as_ptr();
    /// s.advance(60);
    /// assert_eq!(s.capacity(), 4);
    /// assert_eq!(s, [60, 61, 62, 63]);
    ///
    /// // Reclamation of less than 60 bytes succeeds, bringing back the full capacity.
    /// assert!(s.try_reclaim(16));
    /// assert_eq!(s.capacity(), 64);
    /// assert_eq!(s, [60, 61, 62, 63]);
    /// assert_eq!(s.as_ptr(), ptr);
    /// // Trying reclaiming more capacity fails.
    /// assert!(!s.try_reclaim(100));
    /// ```
    pub fn try_reclaim(&mut self, additional: usize) -> bool {
        self.try_reserve_impl(additional, false).is_ok()
    }

    /// Tries reserving capacity for at least `additional` more items, returning an error if the
    /// operation fails.
    ///
    /// Does nothing if the spare capacity is greater than the requested one.
    ///
    /// Reserving is only possible when the `ArcSliceMut` is unique, and when it is supported by
    /// the underlying buffer. It always attempts to [reclaim](Self::try_reclaim) first, and
    /// reallocates the buffer if that fails.
    ///
    /// The default arc-slice buffer supports amortized reservation, doubling the capacity each
    /// time. The reserved capacity might be greater than the requested one.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// # fn main() -> Result<(), arc_slice::error::TryReserveError> {
    /// let mut s = ArcSliceMut::<[u8]>::new();
    /// s.try_reserve(3)?;
    /// assert!(s.capacity() >= 3);
    /// s.extend_from_slice(&[0, 1, 2]);
    /// assert_eq!(s, [0, 1, 2]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.try_reserve_impl(additional, true)
    }

    fn try_reserve_impl(
        &mut self,
        additional: usize,
        allocate: bool,
    ) -> Result<(), TryReserveError> {
        if additional <= self.spare_capacity() {
            return Ok(());
        }
        let res = self.try_reserve_cold(additional, allocate);
        unsafe { assume!(res.is_err() || self.spare_capacity() >= additional) };
        res
    }

    #[cold]
    fn try_reserve_cold(
        &mut self,
        additional: usize,
        allocate: bool,
    ) -> Result<(), TryReserveError> {
        let (capacity, start) = match &mut self.data {
            Some(data) => L::try_reserve::<S, UNIQUE>(
                self.start,
                self.length,
                self.capacity,
                data,
                additional,
                allocate,
            ),
            None if allocate => {
                let capacity = cmp::max(min_non_zero_cap::<S::Item>(), additional);
                let (arc, start) = Arc::<S>::with_capacity::<AllocError, false>(capacity)?;
                self.data = Some(Data(arc.into_raw()));
                (Ok(capacity), start)
            }
            None => return Err(TryReserveError::Unsupported),
        };
        self.start = start;
        self.capacity = capacity?;
        Ok(())
    }

    /// Tries appending a slice to the end of slice, returning an error if the capacity
    /// reservation fails.
    ///
    /// The buffer might have to reserve additional capacity to do the appending.
    ///
    /// The default arc-slice buffer supports amortized reservation, doubling the capacity each
    /// time.
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// # fn main() -> Result<(), arc_slice::error::TryReserveError> {
    /// let mut s = ArcSliceMut::<[u8]>::new();
    /// s.try_extend_from_slice(b"hello world")?;
    /// assert_eq!(s, b"hello world");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_extend_from_slice(&mut self, slice: &S) -> Result<(), TryReserveError>
    where
        S: Concatenable,
        S::Item: Copy,
    {
        self.try_reserve(slice.len())?;
        unsafe { self.extend_from_slice_unchecked(slice.to_slice()) };
        Ok(())
    }

    unsafe fn extend_from_slice_unchecked(&mut self, slice: &[S::Item])
    where
        S: Concatenable,
        S::Item: Copy,
    {
        unsafe {
            let end = self.start.as_ptr().add(self.length);
            ptr::copy_nonoverlapping(slice.as_ptr(), end, slice.len());
            self.length += slice.len();
        }
    }

    /// Advances the start of the slice by `offset` items.
    ///
    /// This operation does not touch the underlying buffer.
    ///
    /// # Panics
    ///
    /// Panics if `offset > self.len()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::from(b"hello world");
    /// s.advance(6);
    /// assert_eq!(s, b"world");
    /// ```
    pub fn advance(&mut self, offset: usize) {
        if offset > self.length {
            panic_out_of_range();
        }
        L::advance::<S, UNIQUE>(self.data.as_mut(), offset);
        self.start = unsafe { self.start.add(offset) };
        self.length -= offset;
        self.capacity -= offset;
    }

    /// Truncate the slice to the first `len` items.
    ///
    /// If `len` is greater than the slice length, this has no effect.
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::from(b"hello world");
    /// s.truncate(5);
    /// assert_eq!(s, b"hello");
    /// ```
    pub fn truncate(&mut self, len: usize) {
        if len >= self.length {
            return;
        }
        if S::needs_drop() {
            let truncate = <L as ArcSliceMutLayout>::truncate::<S, UNIQUE>;
            let data = unsafe { self.data.as_mut().unwrap_unchecked() };
            truncate(self.start, self.length, self.capacity, data);
            // shorten capacity to avoid overwriting droppable items
            self.capacity = len;
        }
        self.length = len;
    }

    /// Accesses the metadata of the underlying buffer if it can be successfully downcast.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSliceMut};
    ///
    /// let metadata = "metadata".to_string();
    /// let s =
    ///     ArcSliceMut::<[u8], ArcLayout<true>>::from_buffer_with_metadata(vec![0, 1, 2], metadata);
    /// assert_eq!(s.metadata::<String>().unwrap(), "metadata");
    /// ```
    pub fn metadata<M: Any>(&self) -> Option<&M> {
        <L as ArcSliceMutLayout>::get_metadata::<S, M, UNIQUE>(self.data.as_ref()?)
    }

    /// Tries downcasting the `ArcSliceMut` to its underlying buffer.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSliceMut};
    ///
    /// let s = ArcSliceMut::<[u8], ArcLayout<true>>::from(vec![0, 1, 2]);
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), [0, 1, 2]);
    /// ```
    pub fn try_into_buffer<B: BufferMut<S>>(self) -> Result<B, Self> {
        // MSRV 1.65 let-else
        let data = match self.data {
            Some(data) => data,
            None => return Err(self),
        };
        let this = ManuallyDrop::new(self);
        let take_buffer = <L as ArcSliceMutLayout>::take_buffer::<S, B, UNIQUE>;
        unsafe { take_buffer(this.start, this.length, this.capacity, data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }

    /// Tries turning the shared `ArcSliceMut` into a unique one.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut a = ArcSliceMut::<[u8]>::from(b"hello world").into_shared();
    /// let b = a.split_to(5);
    /// assert!(a.try_into_unique().is_err());
    /// // a has been dropped
    /// assert!(b.try_into_unique().is_ok());
    /// ```
    #[inline(always)]
    pub fn try_into_unique(mut self) -> Result<ArcSliceMut<S, L, true>, Self> {
        let is_unique = <L as ArcSliceMutLayout>::is_unique::<S, UNIQUE>;
        if !UNIQUE && !self.data.as_mut().is_some_and(is_unique) {
            return Err(self);
        }
        Ok(unsafe { mem::transmute::<Self, ArcSliceMut<S, L, true>>(self) })
    }

    /// Turns the unique `ArcSliceMut` into a shared one.
    ///
    /// Shared `ArcSliceMut` can be split.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut a = ArcSliceMut::<[u8]>::from(b"hello world").into_shared();
    /// let b = a.split_to(5);
    /// ```
    #[inline(always)]
    pub fn into_shared(self) -> ArcSliceMut<S, L, false> {
        unsafe { mem::transmute::<Self, ArcSliceMut<S, L, false>>(self) }
    }

    fn freeze_impl<L2: Layout, E: AllocErrorImpl>(self) -> Result<ArcSlice<S, L2>, Self> {
        let mut this = ManuallyDrop::new(self);
        let frozen_data = L::frozen_data::<S, L2, E, UNIQUE>;
        let data = match this.data {
            Some(data) => frozen_data(this.start, this.length, this.capacity, data),
            None if L2::STATIC_DATA.is_some() || L2::ANY_BUFFER => {
                L2::data_from_static::<_, E>(unsafe { S::from_raw_parts(this.start, this.length) })
                    .ok()
            }
            None => match Arc::new_array::<E, 0>([]) {
                Ok((arc, start)) => {
                    this.start = start;
                    Some(L2::data_from_arc_slice::<S>(arc))
                }
                Err(_) => None,
            },
        };
        match data {
            Some(data) => Ok(ArcSlice::init(this.start, this.length, data)),
            None => Err(ManuallyDrop::into_inner(this)),
        }
    }

    /// Tries freezing the slice, returning an immutable [`ArcSlice`].
    ///
    /// If the mutable slice was split into several parts, only the current one is frozen.
    ///
    /// The conversion may allocate depending on the given [layouts](crate::layout), but allocation
    /// errors are caught and the original slice is returned in this case.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::DefaultLayoutMut, ArcSlice, ArcSliceMut};
    ///
    /// let mut s = ArcSliceMut::<[u8]>::with_capacity(16);
    /// s.extend_from_slice(b"hello world");
    ///
    /// let frozen: ArcSlice<[u8]> = s.try_freeze().unwrap();
    /// ```
    pub fn try_freeze<L2: Layout>(self) -> Result<ArcSlice<S, L2>, Self> {
        self.freeze_impl::<L2, AllocError>()
    }

    fn with_layout_impl<L2: LayoutMut, E: AllocErrorImpl>(
        self,
    ) -> Result<ArcSliceMut<S, L2, UNIQUE>, Self> {
        let this = ManuallyDrop::new(self);
        let update_layout = <L as ArcSliceMutLayout>::update_layout::<S, L2, E, UNIQUE>;
        Ok(ArcSliceMut {
            start: this.start,
            length: this.length,
            capacity: this.capacity,
            data: this
                .data
                .map(|data| update_layout(this.start, this.length, this.capacity, data).ok_or(()))
                .transpose()
                .map_err(|_| ManuallyDrop::into_inner(this))?,
            _phantom: PhantomData,
        })
    }

    /// Tries to replace the layout of the `ArcSliceMut`, returning the original slice if it fails.
    ///
    /// The [layouts](crate::layout) must be compatible for the conversion to succeed, see
    /// [`FromLayout`].
    ///
    /// The conversion may allocate depending on the given [layouts](crate::layout), but allocation
    /// errors are caught and the original slice is also returned in this case.
    ///
    /// # Examples
    /// ```rust
    /// use arc_slice::{
    ///     layout::{ArcLayout, BoxedSliceLayout, VecLayout},
    ///     ArcSliceMut,
    /// };
    ///
    /// let a = ArcSliceMut::<[u8], VecLayout>::from(vec![0, 1, 2]);
    ///
    /// let b = a.try_with_layout::<ArcLayout<true>>().unwrap();
    /// assert!(b.try_with_layout::<ArcLayout<false>>().is_err());
    /// ```
    pub fn try_with_layout<L2: LayoutMut>(self) -> Result<ArcSliceMut<S, L2, UNIQUE>, Self> {
        self.with_layout_impl::<L2, AllocError>()
    }

    /// Converts an `ArcSliceMut` into a primitive `ArcSliceMut`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<str>::from("hello world");
    /// let bytes: ArcSliceMut<[u8]> = s.into_arc_slice_mut();
    /// assert_eq!(bytes, b"hello world");
    /// ```
    pub fn into_arc_slice_mut(self) -> ArcSliceMut<[S::Item], L, UNIQUE> {
        let this = ManuallyDrop::new(self);
        ArcSliceMut {
            start: this.start,
            length: this.length,
            capacity: this.capacity,
            data: this.data,
            _phantom: PhantomData,
        }
    }

    /// Tries converting an item slice into the given `ArcSliceMut`.
    ///
    /// The conversion uses [`Slice::try_from_slice_mut`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let utf8 = ArcSliceMut::<[u8]>::from(b"hello world");
    /// let not_utf8 = ArcSliceMut::<[u8]>::from(b"\x80\x81");
    ///
    /// assert!(ArcSliceMut::<str>::try_from_arc_slice_mut(utf8).is_ok());
    /// assert!(ArcSliceMut::<str>::try_from_arc_slice_mut(not_utf8).is_err());
    /// ```
    #[allow(clippy::type_complexity)]
    pub fn try_from_arc_slice_mut(
        mut slice: ArcSliceMut<[S::Item], L, UNIQUE>,
    ) -> Result<Self, (S::TryFromSliceError, ArcSliceMut<[S::Item], L, UNIQUE>)> {
        match S::try_from_slice_mut(&mut slice) {
            Ok(_) => Ok(unsafe { Self::from_arc_slice_mut_unchecked(slice) }),
            Err(error) => Err((error, slice)),
        }
    }

    /// Converts an item slice into the given `ArcSliceMut`, without checking the slice validity.
    ///
    /// # Safety
    ///
    /// The operation has the same contract as [`Slice::from_slice_mut_unchecked`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let utf8 = ArcSliceMut::<[u8]>::from(b"hello world");
    ///
    /// // SAFETY: `utf8` is a valid utf8 string
    /// let s = unsafe { ArcSliceMut::<str>::from_arc_slice_mut_unchecked(utf8) };
    /// assert_eq!(s, "hello world");
    /// ```
    pub unsafe fn from_arc_slice_mut_unchecked(
        mut slice: ArcSliceMut<[S::Item], L, UNIQUE>,
    ) -> Self {
        debug_assert!(S::try_from_slice_mut(&mut slice).is_ok());
        let slice = ManuallyDrop::new(slice);
        Self {
            start: slice.start,
            length: slice.length,
            capacity: slice.capacity,
            data: slice.data,
            _phantom: slice._phantom,
        }
    }
}

#[cfg(feature = "oom-handling")]
impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> ArcSliceMut<S, L, UNIQUE> {
    /// Freeze the slice, returning an immutable [`ArcSlice`].
    ///
    /// If the mutable slice was split into several parts, only the current one is frozen.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::DefaultLayoutMut, ArcSlice, ArcSliceMut};
    ///
    /// let mut s = ArcSliceMut::<[u8]>::with_capacity(16);
    /// s.extend_from_slice(b"hello world");
    ///
    /// let frozen: ArcSlice<[u8]> = s.freeze();
    /// ```
    pub fn freeze<L2: FromLayout<L>>(self) -> ArcSlice<S, L2> {
        self.freeze_impl::<L2, Infallible>().unwrap_checked()
    }

    /// Replace the layout of the `ArcSliceMut`.
    ///
    /// The [layouts](crate::layout) must be compatible, see [`FromLayout`].
    ///
    /// # Examples
    /// ```rust
    /// use arc_slice::{
    ///     layout::{ArcLayout, BoxedSliceLayout, VecLayout},
    ///     ArcSliceMut,
    /// };
    ///
    /// let a = ArcSliceMut::<[u8]>::from(b"hello world");
    ///
    /// let b = a.with_layout::<VecLayout>();
    /// ```
    pub fn with_layout<L2: LayoutMut + FromLayout<L>>(self) -> ArcSliceMut<S, L2, UNIQUE> {
        self.with_layout_impl::<L2, Infallible>().unwrap_checked()
    }
}

#[cfg(not(feature = "oom-handling"))]
impl<S: Slice + ?Sized, const ANY_BUFFER: bool, const STATIC: bool, const UNIQUE: bool>
    ArcSliceMut<S, ArcLayout<ANY_BUFFER, STATIC>, UNIQUE>
{
    /// Freeze the slice, returning an immutable [`ArcSlice`].
    ///
    /// If the mutable slice was split into several parts, only the current one is frozen.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::DefaultLayoutMut, ArcSlice, ArcSliceMut};
    ///
    /// let mut s = ArcSliceMut::<[u8]>::with_capacity(16);
    /// s.extend_from_slice(b"hello world");
    ///
    /// let frozen: ArcSlice<[u8]> = s.freeze();
    /// ```
    pub fn freeze<L2: FromLayout<ArcLayout<ANY_BUFFER, STATIC>>>(self) -> ArcSlice<S, L2> {
        self.freeze_impl::<L2, Infallible>().unwrap_checked()
    }

    /// Replace the layout of the `ArcSliceMut`.
    ///
    /// The [layouts](crate::layout) must be compatible, see [`FromLayout`].
    ///
    /// # Examples
    /// ```rust
    /// use arc_slice::{
    ///     layout::{ArcLayout, BoxedSliceLayout, VecLayout},
    ///     ArcSliceMut,
    /// };
    ///
    /// let a = ArcSliceMut::<[u8]>::from(b"hello world");
    ///
    /// let b = a.with_layout::<VecLayout>();
    /// ```
    pub fn with_layout<L2: LayoutMut + FromLayout<ArcLayout<ANY_BUFFER, STATIC>>>(
        self,
    ) -> ArcSliceMut<S, L2, UNIQUE> {
        self.with_layout_impl::<L2, Infallible>().unwrap_checked()
    }
}

impl<S: Slice + ?Sized, L: LayoutMut> ArcSliceMut<S, L> {
    pub(crate) const fn init(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Option<Data<true>>,
    ) -> Self {
        Self {
            start,
            length,
            capacity,
            data,
            _phantom: PhantomData,
        }
    }

    /// # Safety
    ///
    /// Empty slice must be valid (see [`Emptyable`])
    const unsafe fn empty() -> Self {
        Self::init(NonNull::dangling(), 0, 0, None)
    }

    /// Creates a new empty `ArcSliceMut`.
    ///
    /// This operation doesn't allocate.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSliceMut};
    ///
    /// let s = ArcSliceMut::<[u8]>::new();
    /// assert_eq!(s, []);
    /// ```
    pub const fn new() -> Self
    where
        S: Emptyable,
    {
        unsafe { Self::empty() }
    }

    pub(crate) fn from_slice_impl<E: AllocErrorImpl>(slice: &S) -> Result<Self, E>
    where
        S::Item: Copy,
    {
        if slice.is_empty() {
            return Ok(unsafe { Self::empty() });
        }
        let (arc, start) = Arc::<S, false>::new::<E>(slice)?;
        Ok(Self::init(
            start,
            slice.len(),
            slice.len(),
            Some(arc.into()),
        ))
    }

    /// Creates a new `ArcSliceMut` by copying the given slice.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::from_slice(b"hello world");
    /// assert_eq!(s, b"hello world");
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_slice(slice: &S) -> Self
    where
        S::Item: Copy,
    {
        Self::from_slice_impl::<Infallible>(slice).unwrap_infallible()
    }

    /// Tries creating a new `ArcSliceMut` by copying the given slice, returning an error if the
    /// allocation fails.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = ArcSliceMut::<[u8]>::try_from_slice(b"hello world")?;
    /// assert_eq!(s, b"hello world");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_from_slice(slice: &S) -> Result<Self, AllocError>
    where
        S::Item: Copy,
    {
        Self::from_slice_impl::<AllocError>(slice)
    }

    #[cfg(feature = "serde")]
    pub(crate) fn new_bytes(slice: &S) -> Self {
        assert_checked(is!(S::Item, u8));
        let (arc, start) = unsafe {
            Arc::<S, false>::new_unchecked::<Infallible>(slice.to_slice()).unwrap_infallible()
        };
        Self::init(start, slice.len(), slice.len(), Some(arc.into()))
    }

    #[cfg(feature = "serde")]
    pub(crate) fn new_byte_vec(vec: S::Vec) -> Self {
        assert_checked(is!(S::Item, u8));
        if !<L as ArcSliceMutLayout>::ANY_BUFFER {
            return Self::new_bytes(ManuallyDrop::new(vec).as_slice());
        }
        Self::from_vec(vec)
    }

    pub(crate) fn from_vec_impl<E: AllocErrorImpl>(mut vec: S::Vec) -> Result<Self, (E, S::Vec)> {
        let capacity = vec.capacity();
        if capacity == 0 {
            return Ok(unsafe { Self::empty() });
        }
        let start = S::vec_start(&mut vec);
        let length = vec.len();
        let data = unsafe { <L as ArcSliceMutLayout>::data_from_vec::<S, E, true>(vec, 0)? };
        Ok(Self::init(start, length, capacity, Some(data)))
    }

    pub(crate) fn from_vec(vec: S::Vec) -> Self {
        Self::from_vec_impl::<Infallible>(vec).unwrap_infallible()
    }

    fn with_capacity_impl<E: AllocErrorImpl, const ZEROED: bool>(
        capacity: usize,
    ) -> Result<Self, E> {
        if capacity == 0 {
            return Ok(unsafe { Self::empty() });
        }
        let (arc, start) = Arc::<S>::with_capacity::<E, ZEROED>(capacity)?;
        let length = if ZEROED { capacity } else { 0 };
        Ok(Self::init(start, length, capacity, Some(arc.into())))
    }

    /// Creates a new `ArcSliceMut` with the given capacity.
    ///
    /// This operation allocates if `capacity > 0`.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::with_capacity(64);
    /// assert_eq!(s, []);
    /// assert_eq!(s.capacity(), 64);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn with_capacity(capacity: usize) -> Self
    where
        S: Emptyable,
    {
        Self::with_capacity_impl::<Infallible, false>(capacity).unwrap_infallible()
    }

    /// Tries creating a new `ArcSliceMut` with the given capacity, returning an error if an
    /// allocation fails.
    ///
    /// This operation allocates if `capacity > 0`.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = ArcSliceMut::<[u8]>::try_with_capacity(64)?;
    /// assert_eq!(s, []);
    /// assert_eq!(s.capacity(), 64);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn try_with_capacity(capacity: usize) -> Result<Self, AllocError>
    where
        S: Emptyable,
    {
        Self::with_capacity_impl::<AllocError, false>(capacity)
    }

    /// Creates a new zeroed `ArcSliceMut` with the given capacity.
    ///
    /// This operation allocates if `capacity > 0`. All the items are initialized to `0`.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::zeroed(4);
    /// assert_eq!(s, [0, 0, 0, 0]);
    /// assert_eq!(s.capacity(), 4);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn zeroed(length: usize) -> Self
    where
        S: Zeroable,
    {
        Self::with_capacity_impl::<Infallible, true>(length).unwrap_infallible()
    }

    /// Tries creating a new zeroed `ArcSliceMut` with the given capacity.
    ///
    /// This operation allocates if `capacity > 0`. All the items are initialized to `0`.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::zeroed(4);
    /// assert_eq!(s, [0, 0, 0, 0]);
    /// assert_eq!(s.capacity(), 4);
    /// ```
    pub fn try_zeroed(length: usize) -> Result<Self, AllocError>
    where
        S: Zeroable,
    {
        Self::with_capacity_impl::<AllocError, true>(length)
    }

    /// Reserve capacity for at least `additional` more items.
    ///
    /// Does nothing if the spare capacity is greater than the requested one.
    ///
    /// Reserving always attempts to [reclaim](Self::try_reclaim) first, and
    /// reallocates the buffer if that fails.
    ///
    /// The default arc-slice buffer supports amortized reservation, doubling the capacity each
    /// time. The reserved capacity might be greater than the requested one.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds isize::MAX bytes, or if the underlying buffer doesn't
    /// support additional capacity reservation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::new();
    /// s.reserve(3);
    /// assert!(s.capacity() >= 3);
    /// s.extend_from_slice(&[0, 1, 2]);
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn reserve(&mut self, additional: usize) {
        if let Err(err) = self.try_reserve(additional) {
            #[cold]
            fn panic_reserve(err: TryReserveError) -> ! {
                match err {
                    TryReserveError::AllocError => {
                        alloc::alloc::handle_alloc_error(core::alloc::Layout::new::<()>())
                    }
                    err => panic!("{err:?}"),
                }
            }
            panic_reserve(err);
        }
    }

    /// Appends an element to the end of the slice.
    ///
    /// The buffer might have to reserve additional capacity to do the appending.
    ///
    /// The default arc-slice buffer supports amortized reservation, doubling the capacity each
    /// time.
    ///
    /// # Panics
    ///
    /// See [reserve](Self::reserve).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::new();
    /// s.push(42);
    /// assert_eq!(s, [42]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn push(&mut self, item: S::Item)
    where
        S: Extendable,
    {
        self.reserve(1);
        unsafe { self.start.as_ptr().add(self.length).write(item) };
        self.length += 1;
    }

    /// Appends a slice to the end of slice.
    ///
    /// The buffer might have to reserve additional capacity to do the appending.
    ///
    /// The default arc-slice buffer supports amortized reservation, doubling the capacity each
    /// time.
    ///
    /// # Panics
    ///
    /// See [reserve](Self::reserve).
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut s = ArcSliceMut::<[u8]>::new();
    /// s.extend_from_slice(b"hello world");
    /// assert_eq!(s, b"hello world");
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn extend_from_slice(&mut self, slice: &S)
    where
        S: Concatenable,
        S::Item: Copy,
    {
        self.reserve(slice.len());
        unsafe { self.extend_from_slice_unchecked(slice.to_slice()) }
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut> ArcSliceMut<[T], L> {
    pub(crate) fn from_array_impl<E: AllocErrorImpl, const N: usize>(
        array: [T; N],
    ) -> Result<Self, (E, [T; N])> {
        if N == 0 {
            return Ok(Self::new());
        }
        let (arc, start) = Arc::<[T], false>::new_array::<E, N>(array)?;
        Ok(Self::init(start, N, N, Some(arc.into())))
    }

    /// Creates a new `ArcSliceMut` by moving the given array.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX - size_of::<usize>()` bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::from_array([0, 1, 2]);
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_array<const N: usize>(array: [T; N]) -> Self {
        Self::from_array_impl::<Infallible, N>(array).unwrap_infallible()
    }

    /// Tries creating a new `ArcSliceMut` by moving the given array,
    /// returning it if an allocation fails.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let s = ArcSliceMut::<[u8]>::try_from_array([0, 1, 2]).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    pub fn try_from_array<const N: usize>(array: [T; N]) -> Result<Self, [T; N]> {
        Self::from_array_impl::<AllocError, N>(array).map_err(|(_, array)| array)
    }
}

impl<S: Slice + ?Sized, L: LayoutMut> ArcSliceMut<S, L, false> {
    unsafe fn clone_impl<E: AllocErrorImpl>(&mut self) -> Result<Self, E> {
        if self.data.is_none() {
            let (arc, start) =
                Arc::<[S::Item], false>::new_array::<E, 0>([]).map_err(|(err, _)| err)?;
            self.start = start;
            self.data = Some(Data(arc.into_raw()));
        }
        <L as ArcSliceMutLayout>::clone::<S, E, false>(
            self.start,
            self.length,
            self.capacity,
            self.data.as_mut().unwrap_checked(),
        )?;
        Ok(Self {
            start: self.start,
            length: self.length,
            capacity: self.capacity,
            data: self.data,
            _phantom: self._phantom,
        })
    }

    fn split_off_impl<E: AllocErrorImpl>(&mut self, at: usize) -> Result<Self, E> {
        if at > self.capacity {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone_impl()? };
        clone.start = unsafe { clone.start.add(at) };
        clone.capacity -= at;
        self.capacity = at;
        if at > self.length {
            clone.length = 0;
        } else {
            self.length = at;
            clone.length -= at;
        }
        Ok(clone)
    }

    /// Tries splitting the slice into two at the given index, returning an error if an allocation
    /// fails.
    ///
    /// Afterwards `self` contains elements `[0, at)`, and the returned `ArcSliceMut`
    /// contains elements `[at, len)`. This operation does not touch the underlying buffer.
    ///
    /// The operation may allocate. See [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout)
    /// documentation for cases where it does not.
    ///
    /// # Panics
    ///
    /// Panics if `at > self.len()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let mut a = ArcSliceMut::<[u8]>::from(b"hello world").into_shared();
    /// let b = a.try_split_off(5)?;
    ///
    /// assert_eq!(a, b"hello");
    /// assert_eq!(b, b" world");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_split_off(&mut self, at: usize) -> Result<Self, AllocError> {
        self.split_off_impl::<AllocError>(at)
    }

    fn split_to_impl<E: AllocErrorImpl>(&mut self, at: usize) -> Result<Self, E> {
        if at > self.length {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone_impl()? };
        clone.capacity = at;
        clone.length = at;
        self.start = unsafe { self.start.add(at) };
        self.capacity -= at;
        self.length -= at;
        Ok(clone)
    }

    /// Tries splitting the slice into two at the given index, returning an error if an allocation
    /// fails.
    ///
    /// Afterwards `self` contains elements `[at, len)`, and the returned `ArcSliceMut`
    /// contains elements `[0, at)`. This operation does not touch the underlying buffer.
    ///
    /// The operation may allocate. See [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout)
    /// documentation for cases where it does not.
    ///
    /// # Panics
    ///
    /// Panics if `at > self.len()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let mut a = ArcSliceMut::<[u8]>::from(b"hello world").into_shared();
    /// let b = a.try_split_to(5)?;
    ///
    /// assert_eq!(a, b" world");
    /// assert_eq!(b, b"hello");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_split_to(&mut self, at: usize) -> Result<Self, AllocError> {
        self.split_to_impl::<AllocError>(at)
    }

    /// Tries unsplitting two parts of a previously split slice.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut a = ArcSliceMut::<[u8]>::from(b"hello world").into_shared();
    ///
    /// let b = a.split_off(5);
    /// assert_eq!(a, b"hello");
    /// assert_eq!(b, b" world");
    /// a.try_unsplit(b).unwrap();
    /// assert_eq!(a, b"hello world");
    ///
    /// assert!(a
    ///     .try_unsplit(ArcSliceMut::from(b"other").into_shared())
    ///     .is_err());
    /// ```
    pub fn try_unsplit(
        &mut self,
        other: ArcSliceMut<S, L, false>,
    ) -> Result<(), ArcSliceMut<S, L, false>> {
        let end = unsafe { self.start.add(self.capacity) };
        if self.length == self.capacity && self.data == other.data && end == other.start {
            self.length += other.length;
            self.capacity += other.capacity;
            return Ok(());
        }
        Err(other)
    }
}

impl<
        S: Slice + ?Sized,
        #[cfg(feature = "oom-handling")] L: LayoutMut,
        #[cfg(not(feature = "oom-handling"))] L: LayoutMut + CloneNoAllocLayout,
    > ArcSliceMut<S, L, false>
{
    /// Splits the slice into two at the given index.
    ///
    /// Afterwards `self` contains elements `[0, at)`, and the returned `ArcSliceMut`
    /// contains elements `[at, len)`. This operation does not touch the underlying buffer.
    ///
    /// # Panics
    ///
    /// Panics if `at > self.capacity()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut a = ArcSliceMut::<[u8]>::from(b"hello world").into_shared();
    /// let b = a.split_off(5);
    ///
    /// assert_eq!(a, b"hello");
    /// assert_eq!(b, b" world");
    /// ```
    #[must_use = "consider `ArcSliceMut::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self {
        self.split_off_impl::<Infallible>(at).unwrap_infallible()
    }

    /// Splits the slice into two at the given index.
    ///
    /// Afterwards `self` contains elements `[at, len)`, and the returned `ArcSliceMut`
    /// contains elements `[0, at)`. This operation does not touch the underlying buffer.
    ///
    /// # Panics
    ///
    /// Panics if `at > self.len()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSliceMut;
    ///
    /// let mut a = ArcSliceMut::<[u8]>::from(b"hello world").into_shared();
    /// let b = a.split_to(5);
    ///
    /// assert_eq!(a, b" world");
    /// assert_eq!(b, b"hello");
    /// ```
    #[must_use = "consider `ArcSliceMut::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self {
        self.split_to_impl::<Infallible>(at).unwrap_infallible()
    }
}

impl<S: Slice + ?Sized, L: AnyBufferLayout + LayoutMut> ArcSliceMut<S, L> {
    pub(crate) fn from_dyn_buffer_impl<B: DynBuffer + BufferMut<S>, E: AllocErrorImpl>(
        buffer: B,
    ) -> Result<Self, (E, B)> {
        let (arc, start, length, capacity) = Arc::new_buffer_mut::<_, E>(buffer)?;
        Ok(Self::init(start, length, capacity, Some(arc.into())))
    }

    fn from_buffer_impl<B: BufferMut<S>, E: AllocErrorImpl>(mut buffer: B) -> Result<Self, (E, B)> {
        match try_transmute::<B, S::Vec>(buffer) {
            Ok(vec) => {
                return Self::from_vec_impl::<E>(vec)
                    .map_err(|(err, v)| (err, transmute_checked(v)))
            }
            Err(b) => buffer = b,
        }
        Self::from_dyn_buffer_impl::<_, E>(BufferWithMetadata::new(buffer, ()))
            .map_err(|(err, b)| (err, b.buffer()))
    }
    /// Creates a new `ArcSliceMut` with the given underlying buffer.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSliceMut};
    ///
    /// let s = ArcSliceMut::<[u8], ArcLayout<true>>::from_buffer(vec![0, 1, 2]);
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_buffer<B: BufferMut<S>>(buffer: B) -> Self {
        Self::from_buffer_impl::<_, Infallible>(buffer).unwrap_infallible()
    }

    /// Tries creating a new `ArcSliceMut` with the given underlying buffer, returning it if an
    /// allocation fails.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer).
    ///
    /// Having an Arc allocation depends on the [layout](crate::layout) and the buffer type,
    /// e.g. there will be no allocation for a `Vec` with [`VecLayout`](crate::layout::VecLayout).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSliceMut};
    ///
    /// let s = ArcSliceMut::<[u8], ArcLayout<true>>::try_from_buffer(vec![0, 1, 2]).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    pub fn try_from_buffer<B: BufferMut<S>>(buffer: B) -> Result<Self, B> {
        Self::from_buffer_impl::<_, AllocError>(buffer).map_err(|(_, buffer)| buffer)
    }

    fn from_buffer_with_metadata_impl<
        B: BufferMut<S>,
        M: Send + Sync + 'static,
        E: AllocErrorImpl,
    >(
        buffer: B,
        metadata: M,
    ) -> Result<Self, (E, (B, M))> {
        if is!(M, ()) {
            return Self::from_buffer_impl::<_, E>(buffer).map_err(|(err, b)| (err, (b, metadata)));
        }
        Self::from_dyn_buffer_impl::<_, E>(BufferWithMetadata::new(buffer, metadata))
            .map_err(|(err, b)| (err, b.into_tuple()))
    }

    /// Creates a new `ArcSliceMut` with the given underlying buffer and its associated metadata.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer);
    /// metadata can be retrieved with [`metadata`](Self::metadata).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSliceMut};
    ///
    /// let metadata = "metadata".to_string();
    /// let s =
    ///     ArcSliceMut::<[u8], ArcLayout<true>>::from_buffer_with_metadata(vec![0, 1, 2], metadata);
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<String>().unwrap(), "metadata");
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    #[cfg(feature = "oom-handling")]
    pub fn from_buffer_with_metadata<B: BufferMut<S>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Self {
        Self::from_buffer_with_metadata_impl::<_, _, Infallible>(buffer, metadata)
            .unwrap_infallible()
    }

    /// Tries creates a new `ArcSliceMut` with the given underlying buffer and its associated metadata,
    /// returning them if an allocation fails.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer);
    /// metadata can be retrieved with [`metadata`](Self::metadata).
    ///
    /// Having an Arc allocation depends on the [layout](crate::layout) and the buffer type,
    /// e.g. there will be no allocation for a `Vec` with [`VecLayout`](crate::layout::VecLayout).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSliceMut};
    ///
    /// let metadata = "metadata".to_string();
    /// let s = ArcSliceMut::<[u8], ArcLayout<true>>::try_from_buffer_with_metadata(
    ///     vec![0, 1, 2],
    ///     metadata,
    /// )
    /// .unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<String>().unwrap(), "metadata");
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    pub fn try_from_buffer_with_metadata<B: BufferMut<S>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Result<Self, (B, M)> {
        Self::from_buffer_with_metadata_impl::<_, _, AllocError>(buffer, metadata)
            .map_err(|(_, bm)| bm)
    }

    /// Creates a new `ArcSliceMut` with the given underlying buffer with borrowed metadata.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer);
    /// metadata can be retrieved with [`metadata`](Self::metadata).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{
    ///     buffer::{BorrowMetadata, Buffer, BufferMut},
    ///     error::TryReserveError,
    ///     layout::ArcLayout,
    ///     ArcSliceMut,
    /// };
    ///
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct MyBuffer(Vec<u8>);
    /// impl Buffer<[u8]> for MyBuffer {
    ///     fn as_slice(&self) -> &[u8] {
    ///         &self.0
    ///     }
    /// }
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct MyMetadata;
    /// impl BorrowMetadata for MyBuffer {
    ///     type Metadata = MyMetadata;
    ///     fn borrow_metadata(&self) -> &Self::Metadata {
    ///         &MyMetadata
    ///     }
    /// }
    /// // SAFETY: `MyBuffer` delegates to `Vec<u8>`, which upholds the invariant
    /// unsafe impl BufferMut<[u8]> for MyBuffer {
    ///     fn as_mut_slice(&mut self) -> &mut [u8] {
    ///         &mut self.0
    ///     }
    ///     fn capacity(&self) -> usize {
    ///         self.0.capacity()
    ///     }
    ///     unsafe fn set_len(&mut self, len: usize) -> bool {
    ///         // SAFETY: same function contract
    ///         unsafe { self.0.set_len(len) };
    ///         true
    ///     }
    ///     fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
    ///         BufferMut::try_reserve(&mut self.0, additional)
    ///     }
    /// }
    /// let buffer = MyBuffer(vec![0, 1, 2]);
    /// let s = ArcSliceMut::<[u8], ArcLayout<true>>::from_buffer_with_borrowed_metadata(buffer);
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<MyMetadata>().unwrap(), &MyMetadata);
    /// assert_eq!(
    ///     s.try_into_buffer::<MyBuffer>().unwrap(),
    ///     MyBuffer(vec![0, 1, 2])
    /// );
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_buffer_with_borrowed_metadata<B: BufferMut<S> + BorrowMetadata>(buffer: B) -> Self {
        Self::from_dyn_buffer_impl::<_, Infallible>(buffer).unwrap_infallible()
    }

    /// Tries creating a new `ArcSliceMut` with the given underlying buffer with borrowed metadata,
    /// returning it if an allocation fails.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer);
    /// metadata can be retrieved with [`metadata`](Self::metadata).
    ///
    /// Having an Arc allocation depends on the [layout](crate::layout) and the buffer type,
    /// e.g. there will be no allocation for a `Vec` with [`VecLayout`](crate::layout::VecLayout).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{
    ///     buffer::{BorrowMetadata, Buffer, BufferMut},
    ///     error::TryReserveError,
    ///     layout::ArcLayout,
    ///     ArcSliceMut,
    /// };
    ///
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct MyBuffer(Vec<u8>);
    /// impl Buffer<[u8]> for MyBuffer {
    ///     fn as_slice(&self) -> &[u8] {
    ///         &self.0
    ///     }
    /// }
    /// // SAFETY: `MyBuffer` delegates to `Vec<u8>`, which upholds the invariant
    /// unsafe impl BufferMut<[u8]> for MyBuffer {
    ///     fn as_mut_slice(&mut self) -> &mut [u8] {
    ///         &mut self.0
    ///     }
    ///     fn capacity(&self) -> usize {
    ///         self.0.capacity()
    ///     }
    ///     unsafe fn set_len(&mut self, len: usize) -> bool {
    ///         // SAFETY: same function contract
    ///         unsafe { self.0.set_len(len) };
    ///         true
    ///     }
    ///     fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
    ///         BufferMut::try_reserve(&mut self.0, additional)
    ///     }
    /// }
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct MyMetadata;
    /// impl BorrowMetadata for MyBuffer {
    ///     type Metadata = MyMetadata;
    ///     fn borrow_metadata(&self) -> &Self::Metadata {
    ///         &MyMetadata
    ///     }
    /// }
    /// let buffer = MyBuffer(vec![0, 1, 2]);
    /// let s = ArcSliceMut::<[u8], ArcLayout<true>>::try_from_buffer_with_borrowed_metadata(buffer)
    ///     .unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<MyMetadata>().unwrap(), &MyMetadata);
    /// assert_eq!(
    ///     s.try_into_buffer::<MyBuffer>().unwrap(),
    ///     MyBuffer(vec![0, 1, 2])
    /// );
    /// ```
    pub fn try_from_buffer_with_borrowed_metadata<B: BufferMut<S> + BorrowMetadata>(
        buffer: B,
    ) -> Result<Self, B> {
        Self::from_dyn_buffer_impl::<_, AllocError>(buffer).map_err(|(_, buffer)| buffer)
    }
}

unsafe impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Send
    for ArcSliceMut<S, L, UNIQUE>
{
}
unsafe impl<S: Slice + ?Sized, L: AnyBufferLayout + LayoutMut, const UNIQUE: bool> Sync
    for ArcSliceMut<S, L, UNIQUE>
{
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Drop for ArcSliceMut<S, L, UNIQUE> {
    fn drop(&mut self) {
        if let Some(data) = self.data {
            let drop = <L as ArcSliceMutLayout>::drop::<S, UNIQUE>;
            unsafe { drop(self.start, self.length, self.capacity, data) };
        }
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Deref for ArcSliceMut<S, L, UNIQUE> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> DerefMut for ArcSliceMut<S, L, UNIQUE> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> AsRef<S> for ArcSliceMut<S, L, UNIQUE> {
    fn as_ref(&self) -> &S {
        self
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> AsMut<S> for ArcSliceMut<S, L, UNIQUE> {
    fn as_mut(&mut self) -> &mut S {
        self
    }
}

impl<S: Hash + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Hash
    for ArcSliceMut<S, L, UNIQUE>
{
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Borrow<S> for ArcSliceMut<S, L, UNIQUE> {
    fn borrow(&self) -> &S {
        self
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> BorrowMut<S>
    for ArcSliceMut<S, L, UNIQUE>
{
    fn borrow_mut(&mut self) -> &mut S {
        self
    }
}

impl<S: Emptyable + ?Sized, L: LayoutMut> Default for ArcSliceMut<S, L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: fmt::Debug + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::Debug
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self.as_slice(), f)
    }
}

impl<S: fmt::Display + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::Display
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::LowerHex
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self.to_slice(), f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::UpperHex
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self.to_slice(), f)
    }
}

impl<S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialEq
    for ArcSliceMut<S, L, UNIQUE>
{
    fn eq(&self, other: &ArcSliceMut<S, L, UNIQUE>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Eq
    for ArcSliceMut<S, L, UNIQUE>
{
}

impl<S: PartialOrd + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialOrd
    for ArcSliceMut<S, L, UNIQUE>
{
    fn partial_cmp(&self, other: &ArcSliceMut<S, L, UNIQUE>) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<S: Ord + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Ord for ArcSliceMut<S, L, UNIQUE> {
    fn cmp(&self, other: &ArcSliceMut<S, L, UNIQUE>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialEq<S>
    for ArcSliceMut<S, L, UNIQUE>
{
    fn eq(&self, other: &S) -> bool {
        self.as_slice() == other
    }
}

impl<'a, S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialEq<&'a S>
    for ArcSliceMut<S, L, UNIQUE>
{
    fn eq(&self, other: &&'a S) -> bool {
        self.as_slice() == *other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool, const N: usize>
    PartialEq<[T; N]> for ArcSliceMut<[T], L, UNIQUE>
{
    fn eq(&self, other: &[T; N]) -> bool {
        *other == **self
    }
}

impl<
        'a,
        T: PartialEq + Send + Sync + 'static,
        L: LayoutMut,
        const UNIQUE: bool,
        const N: usize,
    > PartialEq<&'a [T; N]> for ArcSliceMut<[T], L, UNIQUE>
{
    fn eq(&self, other: &&'a [T; N]) -> bool {
        **other == **self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool, const N: usize>
    PartialEq<ArcSliceMut<[T], L, UNIQUE>> for [T; N]
{
    fn eq(&self, other: &ArcSliceMut<[T], L, UNIQUE>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool>
    PartialEq<ArcSliceMut<[T], L, UNIQUE>> for [T]
{
    fn eq(&self, other: &ArcSliceMut<[T], L, UNIQUE>) -> bool {
        **other == *self
    }
}

impl<L: LayoutMut, const UNIQUE: bool> PartialEq<ArcSliceMut<str, L, UNIQUE>> for str {
    fn eq(&self, other: &ArcSliceMut<str, L, UNIQUE>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> PartialEq<Vec<T>>
    for ArcSliceMut<[T], L, UNIQUE>
{
    fn eq(&self, other: &Vec<T>) -> bool {
        **self == **other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool>
    PartialEq<ArcSliceMut<[T], L, UNIQUE>> for Vec<T>
{
    fn eq(&self, other: &ArcSliceMut<[T], L, UNIQUE>) -> bool {
        **self == **other
    }
}

impl<L: LayoutMut, const UNIQUE: bool> PartialEq<String> for ArcSliceMut<str, L, UNIQUE> {
    fn eq(&self, other: &String) -> bool {
        **self == **other
    }
}

impl<L: LayoutMut, const UNIQUE: bool> PartialEq<ArcSliceMut<str, L, UNIQUE>> for String {
    fn eq(&self, other: &ArcSliceMut<str, L, UNIQUE>) -> bool {
        **self == **other
    }
}

#[cfg(feature = "oom-handling")]
impl<S: Slice + ?Sized, L: LayoutMut> From<&S> for ArcSliceMut<S, L>
where
    S::Item: Copy,
{
    fn from(value: &S) -> Self {
        Self::from_slice(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<T: Copy + Send + Sync + 'static, L: LayoutMut, const N: usize> From<&[T; N]>
    for ArcSliceMut<[T], L>
{
    fn from(value: &[T; N]) -> Self {
        Self::from_slice(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<T: Send + Sync + 'static, L: LayoutMut, const N: usize> From<[T; N]> for ArcSliceMut<[T], L> {
    fn from(value: [T; N]) -> Self {
        Self::from_array(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<T: Send + Sync + 'static, L: AnyBufferLayout + LayoutMut> From<Vec<T>>
    for ArcSliceMut<[T], L>
{
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}

#[cfg(not(feature = "oom-handling"))]
impl<T: Send + Sync + 'static> From<Vec<T>> for ArcSliceMut<[T], VecLayout> {
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const N: usize, const UNIQUE: bool>
    TryFrom<ArcSliceMut<[T], L, UNIQUE>> for [T; N]
{
    type Error = ArcSliceMut<[T], L, UNIQUE>;
    fn try_from(value: ArcSliceMut<[T], L, UNIQUE>) -> Result<Self, Self::Error> {
        let data = match value.data {
            Some(data) => data,
            None if N == 0 => return Ok(transmute_checked::<[T; 0], _>([])),
            None => return Err(value),
        };
        let this = ManuallyDrop::new(value);
        let take_array = <L as ArcSliceMutLayout>::take_array::<T, N, UNIQUE>;
        unsafe { take_array(this.start, this.length, data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }
}

#[cfg(feature = "oom-handling")]
impl<S: Emptyable + Extendable + ?Sized, L: LayoutMut> Extend<S::Item> for ArcSliceMut<S, L> {
    fn extend<I: IntoIterator<Item = S::Item>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().0);
        for item in iter {
            self.push(item);
        }
    }
}

#[cfg(feature = "oom-handling")]
impl<S: Emptyable + Extendable + ?Sized, L: LayoutMut> FromIterator<S::Item> for ArcSliceMut<S, L> {
    fn from_iter<T: IntoIterator<Item = S::Item>>(iter: T) -> Self {
        let mut this = Self::new();
        this.extend(iter);
        this
    }
}

#[cfg(feature = "oom-handling")]
impl<L: LayoutMut> core::str::FromStr for ArcSliceMut<str, L> {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.into())
    }
}

impl<S: Slice<Item = u8> + Extendable + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::Write
    for ArcSliceMut<S, L, UNIQUE>
{
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.try_reserve(s.len()).map_err(|_| fmt::Error)?;
        unsafe { self.extend_from_slice_unchecked(s.as_bytes()) };
        Ok(())
    }

    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::write(self, args)
    }
}

impl<L: LayoutMut, const UNIQUE: bool> fmt::Write for ArcSliceMut<str, L, UNIQUE> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.try_extend_from_slice(s).map_err(|_| fmt::Error)
    }

    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::write(self, args)
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;

    impl<L: LayoutMut, const UNIQUE: bool> std::io::Read for ArcSliceMut<[u8], L, UNIQUE> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = cmp::min(self.len(), buf.len());
            buf[..n].copy_from_slice(&self[..n]);
            Ok(n)
        }
    }

    impl<L: LayoutMut, const UNIQUE: bool> std::io::Write for ArcSliceMut<[u8], L, UNIQUE> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = cmp::min(self.spare_capacity(), buf.len());
            unsafe { self.extend_from_slice_unchecked(&buf[..n]) };
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
};
