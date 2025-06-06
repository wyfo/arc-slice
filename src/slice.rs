use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    any::Any,
    borrow::Borrow,
    cmp,
    convert::Infallible,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, RangeBounds},
    ptr::NonNull,
};

#[cfg(feature = "raw-buffer")]
use crate::buffer::RawBuffer;
#[cfg(not(feature = "oom-handling"))]
use crate::layout::{
    ArcLayout, BoxedSliceLayout, CloneNoAllocLayout, TruncateNoAllocLayout, VecLayout,
};
#[allow(unused_imports)]
use crate::msrv::{ptr, ConstPtrExt, NonNullExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{
        BorrowMetadata, Buffer, BufferExt, BufferMut, BufferWithMetadata, DynBuffer, Emptyable,
        Slice, SliceExt, Subsliceable,
    },
    error::{AllocError, AllocErrorImpl},
    layout::{AnyBufferLayout, DefaultLayout, FromLayout, Layout, LayoutMut, StaticLayout},
    macros::is,
    slice_mut::{ArcSliceMutLayout, Data},
    utils::{
        debug_slice, lower_hex, panic_out_of_range, range_offset_len, subslice_offset_len,
        transmute_checked, try_transmute, upper_hex, UnwrapChecked,
    },
    ArcSliceMut,
};

mod arc;
#[cfg(feature = "raw-buffer")]
mod raw;
mod vec;

#[allow(clippy::missing_safety_doc)]
pub unsafe trait ArcSliceLayout: 'static {
    type Data;
    const ANY_BUFFER: bool;
    const STATIC_DATA: Option<Self::Data>;
    // MSRV 1.83 const `Option::unwrap`
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data>;
    fn data_from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(
        arc: Arc<S, ANY_BUFFER>,
    ) -> Self::Data;
    fn data_from_arc_slice<S: Slice + ?Sized>(arc: Arc<S, false>) -> Self::Data {
        Self::data_from_arc(arc)
    }
    fn data_from_arc_buffer<S: Slice + ?Sized, const ANY_BUFFER: bool, B: DynBuffer + Buffer<S>>(
        arc: Arc<S, ANY_BUFFER>,
    ) -> Self::Data {
        Self::data_from_arc(arc)
    }
    fn try_data_from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(
        arc: ManuallyDrop<Arc<S, ANY_BUFFER>>,
    ) -> Option<Self::Data> {
        Some(Self::data_from_arc(ManuallyDrop::into_inner(arc)))
    }
    fn data_from_static<S: Slice + ?Sized, E: AllocErrorImpl>(
        _slice: &'static S,
    ) -> Result<Self::Data, (E, &'static S)> {
        Ok(Self::STATIC_DATA.unwrap())
    }
    fn data_from_vec<S: Slice + ?Sized, E: AllocErrorImpl>(
        vec: S::Vec,
    ) -> Result<Self::Data, (E, S::Vec)>;
    #[cfg(feature = "raw-buffer")]
    fn data_from_raw_buffer<S: Slice + ?Sized, B: DynBuffer + RawBuffer<S>>(
        _buffer: *const (),
    ) -> Option<Self::Data> {
        None
    }
    fn clone<S: Slice + ?Sized, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        data: &Self::Data,
    ) -> Result<Self::Data, E>;
    unsafe fn drop<S: Slice + ?Sized, const UNIQUE_HINT: bool>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    );
    fn borrowed_data<S: Slice + ?Sized>(_data: &Self::Data) -> Option<*const ()> {
        None
    }
    fn clone_borrowed_data<S: Slice + ?Sized>(_ptr: *const ()) -> Option<Self::Data> {
        None
    }
    fn truncate<S: Slice + ?Sized, E: AllocErrorImpl>(
        _start: NonNull<S::Item>,
        _length: usize,
        _data: &mut Self::Data,
    ) -> Result<(), E> {
        Ok(())
    }
    fn is_unique<S: Slice + ?Sized>(data: &Self::Data) -> bool;
    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Self::Data) -> Option<&M>;
    unsafe fn take_buffer<S: Slice + ?Sized, B: Buffer<S>>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B>;
    unsafe fn take_array<T: Send + Sync + 'static, const N: usize>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<[T; N]>;
    unsafe fn mut_data<S: Slice + ?Sized, L: ArcSliceMutLayout>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<Data>)>;
    fn update_layout<S: Slice + ?Sized, L: ArcSliceLayout, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        data: Self::Data,
    ) -> Option<L::Data>;
}

/// TODO
#[cfg(not(feature = "inlined"))]
pub struct ArcSlice<S: Slice + ?Sized, L: Layout = DefaultLayout> {
    pub(crate) start: NonNull<S::Item>,
    pub(crate) length: usize,
    data: ManuallyDrop<<L as ArcSliceLayout>::Data>,
}

/// TODO
#[cfg(feature = "inlined")]
#[repr(C)]
pub struct ArcSlice<S: Slice + ?Sized, L: Layout = DefaultLayout> {
    #[cfg(target_endian = "big")]
    pub(crate) length: usize,
    data: ManuallyDrop<<L as ArcSliceLayout>::Data>,
    pub(crate) start: NonNull<S::Item>,
    #[cfg(target_endian = "little")]
    pub(crate) length: usize,
}

unsafe impl<S: Slice + ?Sized, L: Layout> Send for ArcSlice<S, L> {}
unsafe impl<S: Slice + ?Sized, L: Layout> Sync for ArcSlice<S, L> {}

impl<S: Slice + ?Sized, L: Layout> ArcSlice<S, L> {
    pub(crate) const fn init(
        start: NonNull<S::Item>,
        length: usize,
        data: <L as ArcSliceLayout>::Data,
    ) -> Self {
        Self {
            start,
            length,
            data: ManuallyDrop::new(data),
        }
    }

    /// Creates a new empty `ArcSlice`.
    ///
    /// This operation doesn't allocate; it is roughly equivalent to `ArcSlice::from_static(&[])`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// let s = ArcSlice::<[u8], ArcLayout<true, true>>::new();
    /// assert_eq!(s, b"");
    /// ```
    pub const fn new() -> Self
    where
        S: Emptyable,
        L: StaticLayout,
    {
        let data = unsafe { L::STATIC_DATA_UNCHECKED.assume_init() };
        Self::init(NonNull::dangling(), 0, data)
    }

    fn from_slice_impl<E: AllocErrorImpl>(slice: &S) -> Result<Self, E>
    where
        S::Item: Copy,
    {
        let (start, length) = slice.to_raw_parts();
        if let Some(empty) = ArcSlice::new_empty(start, length) {
            return Ok(empty);
        }
        let (arc, start) = Arc::<S, false>::new(slice)?;
        Ok(Self::init(start, slice.len(), L::data_from_arc_slice(arc)))
    }

    /// Creates a new `ArcSlice` by copying the given slice.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from_slice(b"hello world");
    /// assert_eq!(s, b"hello world");
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_slice(slice: &S) -> Self
    where
        S::Item: Copy,
    {
        Self::from_slice_impl::<Infallible>(slice).unwrap_checked()
    }

    /// Tries creating a new `ArcSlice` by copying the given slice,
    /// returning an error if the allocation fails.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = ArcSlice::<[u8]>::try_from_slice(b"hello world")?;
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

    fn from_array_impl<E: AllocErrorImpl, const N: usize>(
        array: [S::Item; N],
    ) -> Result<Self, (E, [S::Item; N])> {
        if let Some(empty) = Self::new_empty(NonNull::dangling(), N) {
            return Ok(empty);
        }
        let (arc, start) = Arc::<S, false>::new_array::<E, N>(array)?;
        Ok(Self::init(start, N, L::data_from_arc_slice(arc)))
    }

    #[cfg(feature = "serde")]
    pub(crate) fn new_bytes(slice: &S) -> Self {
        let (start, length) = slice.to_raw_parts();
        if let Some(empty) = ArcSlice::new_empty(start, length) {
            return empty;
        }
        let (arc, start) = unsafe {
            Arc::<S, false>::new_unchecked::<Infallible>(slice.to_slice()).unwrap_checked()
        };
        Self::init(start, slice.len(), L::data_from_arc_slice(arc))
    }

    #[cfg(feature = "serde")]
    pub(crate) fn new_byte_vec(vec: S::Vec) -> Self {
        if !L::ANY_BUFFER {
            return Self::new_bytes(ManuallyDrop::new(vec).as_slice());
        }
        Self::from_vec(vec)
    }

    pub(crate) fn from_vec_impl<E: AllocErrorImpl>(mut vec: S::Vec) -> Result<Self, (E, S::Vec)> {
        if vec.capacity() == 0 {
            return Self::from_array_impl::<E, 0>([]).map_err(|(err, _)| (err, vec));
        }
        let start = S::vec_start(&mut vec);
        Ok(Self::init(start, vec.len(), L::data_from_vec::<S, E>(vec)?))
    }

    pub(crate) fn from_vec(vec: S::Vec) -> Self {
        Self::from_vec_impl::<Infallible>(vec).unwrap_checked()
    }

    fn new_empty(start: NonNull<S::Item>, length: usize) -> Option<Self> {
        let data = L::STATIC_DATA.filter(|_| length == 0)?;
        Some(Self::init(start, length, data))
    }

    /// Returns the number of elements in the slice.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(&[0, 1, 2]);
    /// assert_eq!(s.len(), 3);
    /// ```
    pub const fn len(&self) -> usize {
        self.length
    }

    /// Returns `true` if the slice has a length of 0.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(&[0, 1, 2]);
    /// assert!(!s.is_empty());
    ///
    /// let s = ArcSlice::<[u8]>::from(&[]);
    /// assert!(s.is_empty());
    /// ```
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a raw pointer to the slice’s buffer.
    ///
    /// # Examples
    ///
    /// See [`slice::as_ptr`]
    pub const fn as_ptr(&self) -> *const S::Item {
        self.start.as_ptr()
    }

    /// Extracts a slice containing the entire buffer.
    ///
    /// Equivalent to `&self[..]`.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// assert_eq!(s.as_slice(), b"hello world");
    /// ```
    pub fn as_slice(&self) -> &S {
        unsafe { S::from_raw_parts(self.start, self.length) }
    }

    /// Borrows a subslice of an `ArcSlice` with a given range.
    ///
    /// The returned [`ArcSliceBorrow`] is roughly equivalent to `(&S, &ArcSlice<S, L>)`, but
    /// using [`ArcSliceBorrow::clone_arc`] doesn't need to perform the redundant bound check
    /// when doing the equivalent of [`ArcSlice::subslice_from_ref`].
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let borrow = s.borrow(..5);
    /// assert_eq!(&borrow[..], b"hello");
    /// let s2: ArcSlice<[u8]> = borrow.clone_arc();
    /// ```
    pub fn borrow(&self, range: impl RangeBounds<usize>) -> ArcSliceBorrow<S, L>
    where
        S: Subsliceable,
    {
        unsafe { self.borrow_impl(range_offset_len(self.as_slice(), range)) }
    }

    /// Borrows a subslice of an `ArcSlice` from a slice reference.
    ///
    /// The returned [`ArcSliceBorrow`] is roughly equivalent to `(&S, &ArcSlice<S, L>)`, but
    /// using [`ArcSliceBorrow::clone_arc`] doesn't need to perform the redundant bound check
    /// when doing the equivalent of [`ArcSlice::subslice_from_ref`].
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let hello = &s[..5];
    /// let borrow = s.borrow_from_ref(hello);
    /// assert_eq!(&borrow[..], b"hello");
    /// let s2: ArcSlice<[u8]> = borrow.clone_arc();
    /// ```
    pub fn borrow_from_ref(&self, subset: &S) -> ArcSliceBorrow<S, L>
    where
        S: Subsliceable,
    {
        unsafe { self.borrow_impl(subslice_offset_len(self.as_slice(), subset)) }
    }

    unsafe fn borrow_impl(&self, (offset, len): (usize, usize)) -> ArcSliceBorrow<S, L>
    where
        S: Subsliceable,
    {
        ArcSliceBorrow {
            start: unsafe { self.start.add(offset) },
            length: len,
            ptr: L::borrowed_data::<S>(&self.data).unwrap_or_else(|| ptr::from_ref(self).cast()),
            _phantom: PhantomData,
        }
    }

    fn clone_impl<E: AllocErrorImpl>(&self) -> Result<Self, E> {
        let data = L::clone::<S, E>(self.start, self.length, &self.data)?;
        Ok(Self::init(self.start, self.length, data))
    }

    /// Tries cloning the `ArcSlice`, returning an error if an allocation fails.
    ///
    /// The operation may not allocate, see
    /// [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout) documentation.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = ArcSlice::<[u8]>::try_from_slice(b"hello world")?;
    /// let s2 = s.try_clone()?;
    /// assert_eq!(s2, b"hello world");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_clone(&self) -> Result<Self, AllocError> {
        self.clone_impl::<AllocError>()
    }

    unsafe fn subslice_impl<E: AllocErrorImpl>(
        &self,
        (offset, len): (usize, usize),
    ) -> Result<Self, E>
    where
        S: Subsliceable,
    {
        let start = unsafe { self.start.add(offset) };
        if let Some(empty) = Self::new_empty(start, len) {
            return Ok(empty);
        }
        let mut clone = self.clone_impl::<E>()?;
        clone.start = start;
        clone.length = len;
        Ok(clone)
    }

    /// Tries extracting a subslice of an `ArcSlice` with a given range, returning an error if an allocation fails.
    ///
    /// The operation may not allocate, see
    /// [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout) documentation.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = ArcSlice::<[u8]>::try_from_slice(b"hello world")?;
    /// let s2 = s.try_subslice(..5)?;
    /// assert_eq!(s2, b"hello");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_subslice(&self, range: impl RangeBounds<usize>) -> Result<Self, AllocError>
    where
        S: Subsliceable,
    {
        unsafe { self.subslice_impl::<AllocError>(range_offset_len(self.as_slice(), range)) }
    }

    /// Tries extracting a subslice of an `ArcSlice` from a slice reference, returning an error
    /// if an allocation fails.
    ///
    /// The operation may not allocate, see
    /// [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout) documentation.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let s = ArcSlice::<[u8]>::try_from_slice(b"hello world")?;
    /// let hello = &s[..5];
    /// let s2 = s.try_subslice_from_ref(hello)?;
    /// assert_eq!(s2, b"hello");
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_subslice_from_ref(&self, subset: &S) -> Result<Self, AllocError>
    where
        S: Subsliceable,
    {
        unsafe { self.subslice_impl::<AllocError>(subslice_offset_len(self.as_slice(), subset)) }
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
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let mut s = ArcSlice::<[u8]>::from(b"hello world");
    /// s.advance(6);
    /// assert_eq!(s, b"world");
    /// ```
    pub fn advance(&mut self, offset: usize)
    where
        S: Subsliceable,
    {
        if offset > self.length {
            panic_out_of_range();
        }
        unsafe { self.check_advance(offset) };
        self.start = unsafe { self.start.add(offset) };
        self.length -= offset;
    }

    fn truncate_impl<E: AllocErrorImpl>(&mut self, len: usize) -> Result<(), E>
    where
        S: Subsliceable,
    {
        if len < self.length {
            unsafe { self.check_truncate(len) };
            L::truncate::<S, E>(self.start, self.length, &mut self.data)?;
            self.length = len;
        }
        Ok(())
    }

    /// Tries truncating the slice to the first `len` items, returning an error if an
    /// allocation fails.
    ///
    /// If `len` is greater than the slice length, this has no effect.
    ///
    /// The operation may not allocate, see
    /// [`TruncateNoAllocLayout`](crate::layout::TruncateNoAllocLayout) documentation.
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let mut s = ArcSlice::<[u8]>::from(b"hello world");
    /// s.try_truncate(5)?;
    /// assert_eq!(s, b"hello");
    /// # Ok(())
    /// }
    /// ```
    pub fn try_truncate(&mut self, len: usize) -> Result<(), AllocError>
    where
        S: Subsliceable,
    {
        self.truncate_impl::<AllocError>(len)
    }

    fn split_off_impl<E: AllocErrorImpl>(&mut self, at: usize) -> Result<Self, E>
    where
        S: Subsliceable,
    {
        if at == 0 {
            return Ok(mem::replace(self, unsafe { self.subslice_impl((0, 0))? }));
        } else if at == self.length {
            return unsafe { self.subslice_impl((at, 0)) };
        } else if at > self.length {
            panic_out_of_range();
        }
        let mut clone = self.clone_impl()?;
        clone.start = unsafe { clone.start.add(at) };
        clone.length -= at;
        self.length = at;
        Ok(clone)
    }

    /// Try splitting the slice into two at the given index, returning an error if an allocation
    /// fails.
    ///
    /// Afterwards `self` contains elements `[0, at)`, and the returned `ArcSlice`
    /// contains elements `[at, len)`. This operation does not touch the underlying buffer.
    ///
    /// The operation may not allocate, see
    /// [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout) documentation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let mut a = ArcSlice::<[u8]>::from(b"hello world");
    /// let b = a.try_split_off(5)?;
    ///
    /// assert_eq!(a, b"hello");
    /// assert_eq!(b, b" world");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `at > self.len()`.
    pub fn try_split_off(&mut self, at: usize) -> Result<Self, AllocError>
    where
        S: Subsliceable,
    {
        self.split_off_impl::<AllocError>(at)
    }

    fn split_to_impl<E: AllocErrorImpl>(&mut self, at: usize) -> Result<Self, E>
    where
        S: Subsliceable,
    {
        if at == 0 {
            return unsafe { self.subslice_impl((0, 0)) };
        } else if at == self.length {
            return Ok(mem::replace(self, unsafe {
                self.subslice_impl((self.len(), 0))?
            }));
        } else if at > self.length {
            panic_out_of_range();
        }
        let mut clone = self.clone_impl()?;
        clone.length = at;
        self.start = unsafe { self.start.add(at) };
        self.length -= at;
        Ok(clone)
    }

    /// Try splitting the slice into two at the given index, returning an error if an allocation
    /// fails.
    ///
    /// Afterwards `self` contains elements `[at, len)`, and the returned `ArcSlice`
    /// contains elements `[0, at)`. This operation does not touch the underlying buffer.
    ///
    /// The operation may not allocate, see
    /// [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout) documentation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// # fn main() -> Result<(), arc_slice::error::AllocError> {
    /// let mut a = ArcSlice::<[u8]>::from(b"hello world");
    /// let b = a.try_split_to(5)?;
    ///
    /// assert_eq!(a, b" world");
    /// assert_eq!(b, b"hello");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `at > self.len()`.
    pub fn try_split_to(&mut self, at: usize) -> Result<Self, AllocError>
    where
        S: Subsliceable,
    {
        self.split_to_impl::<AllocError>(at)
    }

    /// Tries to acquire the slice as mutable, returning an [`ArcSliceMut`] on success.
    ///
    /// There must be no other reference to the underlying buffer, and this one must be mutable
    /// for the conversion to succeed. Otherwise, the original slice is returned. An `ArcSlice`
    /// created from an array/slice or a vector is guaranteed to have a mutable buffer, as well
    /// as one returned [`ArcSliceMut::freeze`].
    ///
    /// The conversion may allocate depending on the given [layouts](crate::layout), but allocation
    /// errors are caught and the original slice is also returned in this case.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::DefaultLayout, ArcSlice, ArcSliceMut};
    ///
    /// let mut a = ArcSlice::<[u8]>::from(b"hello world");
    /// let b = a.clone();
    ///
    /// assert!(b.try_into_mut::<DefaultLayout>().is_err());
    /// // b has been dropped
    /// let a_mut: ArcSliceMut<[u8]> = a.try_into_mut().unwrap();
    /// ```
    pub fn try_into_mut<L2: LayoutMut>(self) -> Result<ArcSliceMut<S, L2>, Self> {
        let mut this = ManuallyDrop::new(self);
        match unsafe { L::mut_data::<S, L2>(this.start, this.length, &mut this.data) } {
            Some((capacity, data)) => {
                Ok(ArcSliceMut::init(this.start, this.length, capacity, data))
            }
            None => Err(ManuallyDrop::into_inner(this)),
        }
    }

    /// Returns `true` if this is the only reference to the underlying buffer, and if this one
    /// is unique (see [`Buffer::is_unique`]).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// assert!(s.is_unique());
    /// let s2 = s.clone();
    /// assert!(!s.is_unique());
    /// drop(s2);
    /// assert!(s.is_unique());
    /// ```
    pub fn is_unique(&self) -> bool {
        L::is_unique::<S>(&self.data)
    }

    /// Accesses the metadata of the underlying buffer if it can be successfully downcasted.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// let metadata = "metadata".to_string();
    /// let s = ArcSlice::<[u8], ArcLayout<true>>::from_buffer_with_metadata(vec![0, 1, 2], metadata);
    /// assert_eq!(s.metadata::<String>().unwrap(), "metadata");
    /// ```
    pub fn metadata<M: Any>(&self) -> Option<&M> {
        L::get_metadata::<S, M>(&self.data)
    }

    /// Tries downcasting the `ArcSlice` to its underlying buffer.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// let s = ArcSlice::<[u8], ArcLayout<true>>::from(vec![0, 1, 2]);
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), [0, 1, 2]);
    /// ```
    pub fn try_into_buffer<B: Buffer<S>>(self) -> Result<B, Self> {
        let mut this = ManuallyDrop::new(self);
        unsafe { L::take_buffer::<S, B>(this.start, this.length, &mut this.data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }

    fn with_layout_impl<L2: Layout, E: AllocErrorImpl>(self) -> Result<ArcSlice<S, L2>, Self> {
        let mut this = ManuallyDrop::new(self);
        let data = unsafe { ManuallyDrop::take(&mut this.data) };
        match L::update_layout::<S, L2, E>(this.start, this.length, data) {
            Some(data) => Ok(ArcSlice::init(this.start, this.len(), data)),
            None => Err(ManuallyDrop::into_inner(this)),
        }
    }

    /// Tries to replace the layout of the `ArcSlice`, returning the original slice if it fails.
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
    ///     ArcSlice,
    /// };
    ///
    /// let a = ArcSlice::<[u8], BoxedSliceLayout>::from(vec![0, 1, 2]);
    ///
    /// let b = a.try_with_layout::<VecLayout>().unwrap();
    /// assert!(b.try_with_layout::<ArcLayout<false>>().is_err());
    /// ```
    pub fn try_with_layout<L2: Layout>(self) -> Result<ArcSlice<S, L2>, Self> {
        self.with_layout_impl::<L2, AllocError>()
    }

    /// Converts an `ArcSlice` into a primitive `ArcSlice`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<str>::from("hello world");
    /// let bytes: ArcSlice<[u8]> = s.into_arc_slice();
    /// assert_eq!(bytes, b"hello world");
    /// ```
    pub fn into_arc_slice(self) -> ArcSlice<[S::Item], L> {
        let mut this = ManuallyDrop::new(self);
        ArcSlice {
            start: this.start,
            length: this.length,
            data: ManuallyDrop::new(unsafe { ManuallyDrop::take(&mut this.data) }),
        }
    }

    /// Tries converting an item slice into the given `ArcSlice`.
    ///
    /// The conversion uses [`Slice::try_from_slice`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let utf8 = ArcSlice::<[u8]>::from(b"hello world");
    /// let not_utf8 = ArcSlice::<[u8]>::from(b"\x80\x81");
    ///
    /// assert!(ArcSlice::<str>::try_from_arc_slice(utf8).is_ok());
    /// assert!(ArcSlice::<str>::try_from_arc_slice(not_utf8).is_err());
    /// ```
    #[allow(clippy::type_complexity)]
    pub fn try_from_arc_slice(
        slice: ArcSlice<[S::Item], L>,
    ) -> Result<Self, (S::TryFromSliceError, ArcSlice<[S::Item], L>)> {
        match S::try_from_slice(&slice) {
            Ok(_) => Ok(unsafe { Self::from_arc_slice_unchecked(slice) }),
            Err(error) => Err((error, slice)),
        }
    }

    /// Convert an item slice into the given `ArcSlice`, without checking the slice validity.
    ///
    /// # Safety
    ///
    /// The operation has the same contract as [`Slice::from_slice_unchecked`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let utf8 = ArcSlice::<[u8]>::from(b"hello world");
    /// let not_utf8 = ArcSlice::<[u8]>::from(b"\x80\x81");
    ///
    /// assert!(ArcSlice::<str>::try_from_arc_slice(utf8).is_ok());
    /// assert!(ArcSlice::<str>::try_from_arc_slice(not_utf8).is_err());
    /// ```
    pub unsafe fn from_arc_slice_unchecked(slice: ArcSlice<[S::Item], L>) -> Self {
        debug_assert!(S::try_from_slice(&slice).is_ok());
        let mut slice = ManuallyDrop::new(slice);
        Self {
            start: slice.start,
            length: slice.length,
            data: ManuallyDrop::new(unsafe { ManuallyDrop::take(&mut slice.data) }),
        }
    }

    /// Drops an `ArcSlice`, hinting that it should be unique.
    ///
    /// In case of actual unicity, this method should be a little bit more efficient than a
    /// conventional drop. Indeed, in the case where an Arc has been allocated, it will first
    /// check the refcount, and shortcut the atomic fetch-and-sub if the count is one.
    pub fn drop_with_unique_hint(self) {
        let mut this = ManuallyDrop::new(self);
        unsafe { L::drop::<S, true>(this.start, this.length, &mut this.data) };
    }
}

impl<T: Send + Sync + 'static, L: Layout> ArcSlice<[T], L> {
    /// Creates a new `ArcSlice` by moving the given array.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from_array([0, 1, 2]);
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_array<const N: usize>(array: [T; N]) -> Self {
        Self::from_array_impl::<Infallible, N>(array).unwrap_checked()
    }

    /// Tries creating a new `ArcSlice` by moving the given array,
    /// returning it if an allocation fails.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::try_from_array([0, 1, 2]).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// ```
    pub fn try_from_array<const N: usize>(array: [T; N]) -> Result<Self, [T; N]> {
        Self::from_array_impl::<AllocError, N>(array).map_err(|(_, array)| array)
    }
}

impl<
        S: Slice + ?Sized,
        #[cfg(feature = "oom-handling")] L: Layout,
        #[cfg(not(feature = "oom-handling"))] L: TruncateNoAllocLayout,
    > ArcSlice<S, L>
{
    /// Truncate the slice to the first `len` items.
    ///
    /// If `len` is greater than the slice length, this has no effect.
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let mut s = ArcSlice::<[u8]>::from(b"hello world");
    /// s.truncate(5);
    /// assert_eq!(s, b"hello");
    /// ```
    pub fn truncate(&mut self, len: usize)
    where
        S: Subsliceable,
    {
        self.truncate_impl::<Infallible>(len).unwrap_checked();
    }
}

impl<
        S: Slice + ?Sized,
        #[cfg(feature = "oom-handling")] L: Layout,
        #[cfg(not(feature = "oom-handling"))] L: CloneNoAllocLayout,
    > ArcSlice<S, L>
{
    /// Extracts a subslice of an `ArcSlice` with a given range.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let s2 = s.subslice(..5);
    /// assert_eq!(s2, b"hello");
    /// ```
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self
    where
        S: Subsliceable,
    {
        unsafe { self.subslice_impl::<Infallible>(range_offset_len(self.as_slice(), range)) }
            .unwrap_checked()
    }

    /// Extracts a subslice of an `ArcSlice` from a slice reference.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let hello = &s[..5];
    /// let s2 = s.subslice_from_ref(hello);
    /// assert_eq!(s2, b"hello");
    /// ```
    pub fn subslice_from_ref(&self, subset: &S) -> Self
    where
        S: Subsliceable,
    {
        unsafe { self.subslice_impl::<Infallible>(subslice_offset_len(self.as_slice(), subset)) }
            .unwrap_checked()
    }

    /// Splits the slice into two at the given index.
    ///
    /// Afterwards `self` contains elements `[0, at)`, and the returned `ArcSlice`
    /// contains elements `[at, len)`. This operation does not touch the underlying buffer.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let mut a = ArcSlice::<[u8]>::from(b"hello world");
    /// let b = a.split_off(5);
    ///
    /// assert_eq!(a, b"hello");
    /// assert_eq!(b, b" world");
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `at > self.len()`.
    #[must_use = "consider `ArcSlice::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self
    where
        S: Subsliceable,
    {
        self.split_off_impl::<Infallible>(at).unwrap_checked()
    }

    /// Splits the slice into two at the given index.
    ///
    /// Afterwards `self` contains elements `[at, len)`, and the returned `ArcSlice`
    /// contains elements `[0, at)`. This operation does not touch the underlying buffer.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let mut a = ArcSlice::<[u8]>::from(b"hello world");
    /// let b = a.split_to(5);
    ///
    /// assert_eq!(a, b" world");
    /// assert_eq!(b, b"hello");
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `at > self.len()`.
    #[must_use = "consider `ArcSlice::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self
    where
        S: Subsliceable,
    {
        self.split_to_impl::<Infallible>(at).unwrap_checked()
    }
}

#[cfg(feature = "oom-handling")]
impl<S: Slice + ?Sized, L: Layout> ArcSlice<S, L> {
    /// Replace the layout of the `ArcSlice`.
    ///
    /// The [layouts](crate::layout) must be compatible, see [`FromLayout`].
    ///
    /// # Examples
    /// ```rust
    /// use arc_slice::{
    ///     layout::{ArcLayout, BoxedSliceLayout, VecLayout},
    ///     ArcSlice,
    /// };
    ///
    /// let a = ArcSlice::<[u8]>::from(b"hello world");
    ///
    /// let b = a.with_layout::<VecLayout>();
    /// ```
    pub fn with_layout<L2: FromLayout<L>>(self) -> ArcSlice<S, L2> {
        self.with_layout_impl::<L2, Infallible>().unwrap_checked()
    }
}

#[cfg(not(feature = "oom-handling"))]
impl<S: Slice + ?Sized, const ANY_BUFFER: bool, const STATIC: bool>
    ArcSlice<S, ArcLayout<ANY_BUFFER, STATIC>>
{
    /// Replace the layout of the `ArcSlice`.
    ///
    /// The [layouts](crate::layout) must be compatible, see [`FromLayout`].
    ///
    /// # Examples
    /// ```rust
    /// use arc_slice::{
    ///     layout::{ArcLayout, BoxedSliceLayout, VecLayout},
    ///     ArcSlice,
    /// };
    ///
    /// let a = ArcSlice::<[u8]>::from(b"hello world");
    ///
    /// let b = a.with_layout::<VecLayout>();
    /// ```
    pub fn with_layout<L2: FromLayout<ArcLayout<ANY_BUFFER, STATIC>>>(self) -> ArcSlice<S, L2> {
        self.with_layout_impl::<L2, Infallible>().unwrap_checked()
    }
}

impl<S: Slice + ?Sized, L: AnyBufferLayout> ArcSlice<S, L> {
    pub(crate) fn from_dyn_buffer_impl<B: DynBuffer + Buffer<S>, E: AllocErrorImpl>(
        buffer: B,
    ) -> Result<Self, (E, B)> {
        let (arc, start, length) = Arc::new_buffer::<_, E>(buffer)?;
        let data = L::data_from_arc_buffer::<S, true, B>(arc);
        Ok(Self::init(start, length, data))
    }

    pub(crate) fn from_static_impl<E: AllocErrorImpl>(
        slice: &'static S,
    ) -> Result<Self, (E, &'static S)> {
        let (start, length) = slice.to_raw_parts();
        Ok(Self::init(
            start,
            length,
            L::data_from_static::<_, E>(slice)?,
        ))
    }

    fn from_buffer_impl<B: Buffer<S>, E: AllocErrorImpl>(mut buffer: B) -> Result<Self, (E, B)> {
        match try_transmute::<B, &'static S>(buffer) {
            Ok(slice) => {
                return Self::from_static_impl::<E>(slice)
                    .map_err(|(err, s)| (err, transmute_checked(s)))
            }
            Err(b) => buffer = b,
        }
        match try_transmute::<B, Box<S>>(buffer) {
            Ok(boxed) => {
                let vec = unsafe { S::from_vec_unchecked(boxed.into_boxed_slice().into_vec()) };
                return match Self::from_vec_impl::<E>(vec) {
                    Ok(this) => Ok(this),
                    Err((err, vec)) => Err((
                        err,
                        transmute_checked(unsafe {
                            S::from_boxed_slice_unchecked(S::into_vec(vec).into_boxed_slice())
                        }),
                    )),
                };
            }
            Err(b) => buffer = b,
        }
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

    /// Creates a new `ArcSlice` with the given underlying buffer.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// let s = ArcSlice::<[u8], ArcLayout<true>>::from_buffer(vec![0, 1, 2]);
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_buffer<B: Buffer<S>>(buffer: B) -> Self {
        Self::from_buffer_impl::<_, Infallible>(buffer).unwrap_checked()
    }

    /// Tries creating a new `ArcSlice` with the given underlying buffer, returning it if an
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
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// let s = ArcSlice::<[u8], ArcLayout<true>>::try_from_buffer(vec![0, 1, 2]).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    pub fn try_from_buffer<B: Buffer<S>>(buffer: B) -> Result<Self, B> {
        Self::from_buffer_impl::<_, AllocError>(buffer).map_err(|(_, buffer)| buffer)
    }

    fn from_buffer_with_metadata_impl<B: Buffer<S>, M: Send + Sync + 'static, E: AllocErrorImpl>(
        buffer: B,
        metadata: M,
    ) -> Result<Self, (E, (B, M))> {
        if is!(M, ()) {
            return Self::from_buffer_impl::<_, E>(buffer).map_err(|(err, b)| (err, (b, metadata)));
        }
        Self::from_dyn_buffer_impl::<_, E>(BufferWithMetadata::new(buffer, metadata))
            .map_err(|(err, b)| (err, b.into_tuple()))
    }

    /// Creates a new `ArcSlice` with the given underlying buffer and its associated metadata.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer);
    /// metadata can be retrieved with [`metadata`](Self::metadata).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// let metadata = "metadata".to_string();
    /// let s = ArcSlice::<[u8], ArcLayout<true>>::from_buffer_with_metadata(vec![0, 1, 2], metadata);
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<String>().unwrap(), "metadata");
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_buffer_with_metadata<B: Buffer<S>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Self {
        Self::from_buffer_with_metadata_impl::<_, _, Infallible>(buffer, metadata).unwrap_checked()
    }

    /// Tries creates a new `ArcSlice` with the given underlying buffer and its associated metadata,
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
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// let metadata = "metadata".to_string();
    /// let s =
    ///     ArcSlice::<[u8], ArcLayout<true>>::try_from_buffer_with_metadata(vec![0, 1, 2], metadata)
    ///         .unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<String>().unwrap(), "metadata");
    /// assert_eq!(s.try_into_buffer::<Vec<u8>>().unwrap(), vec![0, 1, 2]);
    /// ```
    pub fn try_from_buffer_with_metadata<B: Buffer<S>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Result<Self, (B, M)> {
        Self::from_buffer_with_metadata_impl::<_, _, AllocError>(buffer, metadata)
            .map_err(|(_, bm)| bm)
    }

    /// Creates a new `ArcSlice` with the given underlying buffer with borrowed metadata.
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer);
    /// metadata can be retrieved with [`metadata`](Self::metadata).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{
    ///     buffer::{BorrowMetadata, Buffer},
    ///     layout::ArcLayout,
    ///     ArcSlice,
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
    /// let buffer = MyBuffer(vec![0, 1, 2]);
    /// let s = ArcSlice::<[u8], ArcLayout<true>>::from_buffer_with_borrowed_metadata(buffer);
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<MyMetadata>().unwrap(), &MyMetadata);
    /// assert_eq!(
    ///     s.try_into_buffer::<MyBuffer>().unwrap(),
    ///     MyBuffer(vec![0, 1, 2])
    /// );
    /// ```
    #[cfg(feature = "oom-handling")]
    pub fn from_buffer_with_borrowed_metadata<B: Buffer<S> + BorrowMetadata>(buffer: B) -> Self {
        Self::from_dyn_buffer_impl::<_, Infallible>(buffer).unwrap_checked()
    }

    /// Tries creating a new `ArcSlice` with the given underlying buffer with borrowed metadata,
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
    ///     buffer::{BorrowMetadata, Buffer},
    ///     layout::ArcLayout,
    ///     ArcSlice,
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
    /// let buffer = MyBuffer(vec![0, 1, 2]);
    /// let s =
    ///     ArcSlice::<[u8], ArcLayout<true>>::try_from_buffer_with_borrowed_metadata(buffer).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<MyMetadata>().unwrap(), &MyMetadata);
    /// assert_eq!(
    ///     s.try_into_buffer::<MyBuffer>().unwrap(),
    ///     MyBuffer(vec![0, 1, 2])
    /// );
    /// ```
    pub fn try_from_buffer_with_borrowed_metadata<B: Buffer<S> + BorrowMetadata>(
        buffer: B,
    ) -> Result<Self, B> {
        Self::from_dyn_buffer_impl::<_, AllocError>(buffer).map_err(|(_, buffer)| buffer)
    }

    #[cfg(feature = "raw-buffer")]
    fn from_raw_buffer_impl<B: DynBuffer + RawBuffer<S>, E: AllocErrorImpl>(
        buffer: B,
    ) -> Result<Self, (E, B)> {
        let ptr = buffer.into_raw();
        if let Some(data) = L::data_from_raw_buffer::<S, B>(ptr) {
            let buffer = ManuallyDrop::new(unsafe { B::from_raw(ptr) });
            let (start, length) = buffer.as_slice().to_raw_parts();
            return Ok(Self::init(start, length, data));
        }
        Self::from_dyn_buffer_impl::<_, E>(unsafe { B::from_raw(ptr) })
    }

    /// Creates a new `ArcSlice` with the given underlying raw buffer.
    ///
    /// For [layouts](crate::layout) others than [`RawLayout`](crate::layout::RawLayout), it is
    /// the same as [`from_buffer`](Self::from_buffer).
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use arc_slice::{layout::RawLayout, ArcSlice};
    ///
    /// let s = ArcSlice::<[u8], RawLayout>::from_raw_buffer(Arc::new(vec![0, 1, 2]));
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(
    ///     s.try_into_buffer::<Arc<Vec<u8>>>().unwrap(),
    ///     Arc::new(vec![0, 1, 2])
    /// );
    /// ```
    #[cfg(all(feature = "raw-buffer", feature = "oom-handling"))]
    pub fn from_raw_buffer<B: RawBuffer<S>>(buffer: B) -> Self {
        Self::from_raw_buffer_impl::<_, Infallible>(BufferWithMetadata::new(buffer, ()))
            .unwrap_checked()
    }

    /// Tries creating a new `ArcSlice` with the given underlying raw buffer, returning it if an
    /// allocation fails.
    ///
    /// For [layouts](crate::layout) others than [`RawLayout`](crate::layout::RawLayout), it is
    /// the same as [`try_from_buffer`](Self::try_from_buffer).
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use arc_slice::{layout::RawLayout, ArcSlice};
    ///
    /// let s = ArcSlice::<[u8], RawLayout>::try_from_raw_buffer(Arc::new(vec![0, 1, 2])).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(
    ///     s.try_into_buffer::<Arc<Vec<u8>>>().unwrap(),
    ///     Arc::new(vec![0, 1, 2])
    /// );
    /// ```
    #[cfg(feature = "raw-buffer")]
    pub fn try_from_raw_buffer<B: RawBuffer<S>>(buffer: B) -> Result<Self, B> {
        Self::from_raw_buffer_impl::<_, AllocError>(BufferWithMetadata::new(buffer, ()))
            .map_err(|(_, b)| b.buffer())
    }

    /// Creates a new `ArcSlice` with the given underlying raw buffer with borrowed metadata.
    ///
    /// For [layouts](crate::layout) others than [`RawLayout`](crate::layout::RawLayout), it is
    /// the same as [`from_buffer`](Self::from_buffer).
    ///
    /// The buffer can be extracted back using [`try_into_buffer`](Self::try_into_buffer);
    /// metadata can be retrieved with [`metadata`](Self::metadata).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// ///
    /// use arc_slice::buffer::{BorrowMetadata, Buffer};
    /// use arc_slice::{layout::RawLayout, ArcSlice};
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
    ///
    /// let buffer = Arc::new(MyBuffer(vec![0, 1, 2]));
    /// let s = ArcSlice::<[u8], RawLayout>::from_raw_buffer_with_borrowed_metadata(buffer);
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<MyMetadata>().unwrap(), &MyMetadata);
    /// assert_eq!(
    ///     s.try_into_buffer::<Arc<MyBuffer>>().unwrap(),
    ///     Arc::new(MyBuffer(vec![0, 1, 2]))
    /// );
    /// ```
    #[cfg(all(feature = "raw-buffer", feature = "oom-handling"))]
    pub fn from_raw_buffer_with_borrowed_metadata<B: RawBuffer<S> + BorrowMetadata>(
        buffer: B,
    ) -> Self {
        Self::from_dyn_buffer_impl::<_, Infallible>(buffer).unwrap_checked()
    }

    /// Tries creating a new `ArcSlice` with the given underlying raw buffer with borrowed metadata,
    /// returning it if an allocation fails.
    ///
    /// For [layouts](crate::layout) others than [`RawLayout`](crate::layout::RawLayout), it is
    /// the same as [`from_buffer`](Self::from_buffer).
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
    /// use std::sync::Arc;
    ///
    /// ///
    /// use arc_slice::buffer::{BorrowMetadata, Buffer};
    /// use arc_slice::{layout::RawLayout, ArcSlice};
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
    ///
    /// let buffer = Arc::new(MyBuffer(vec![0, 1, 2]));
    /// let s =
    ///     ArcSlice::<[u8], RawLayout>::try_from_raw_buffer_with_borrowed_metadata(buffer).unwrap();
    /// assert_eq!(s, [0, 1, 2]);
    /// assert_eq!(s.metadata::<MyMetadata>().unwrap(), &MyMetadata);
    /// assert_eq!(
    ///     s.try_into_buffer::<Arc<MyBuffer>>().unwrap(),
    ///     Arc::new(MyBuffer(vec![0, 1, 2]))
    /// );
    /// ```
    #[cfg(feature = "raw-buffer")]
    pub fn try_from_raw_buffer_with_borrowed_metadata<B: RawBuffer<S> + BorrowMetadata>(
        buffer: B,
    ) -> Result<Self, B> {
        Self::from_dyn_buffer_impl::<_, AllocError>(buffer).map_err(|(_, buffer)| buffer)
    }
}

impl<L: StaticLayout> ArcSlice<[u8], L> {
    /// Creates a new `ArcSlice` from a static slice.
    ///
    /// The operation never allocates.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// static HELLO_WORLD: ArcSlice<[u8], ArcLayout<true, true>> =
    ///     ArcSlice::<[u8], ArcLayout<true, true>>::from_static(b"hello world");
    /// ```
    pub const fn from_static(slice: &'static [u8]) -> Self {
        // MSRV 1.65 const `<*const _>::cast_mut` + 1.85 const `NonNull::new`
        let start = unsafe { NonNull::new_unchecked(slice.as_ptr() as _) };
        let length = slice.len();
        let data = unsafe { L::STATIC_DATA_UNCHECKED.assume_init() };
        Self::init(start, length, data)
    }
}

impl<L: StaticLayout> ArcSlice<str, L> {
    /// Creates a new `ArcSlice` from a static str.
    ///
    /// The operation never allocates.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::{layout::ArcLayout, ArcSlice};
    ///
    /// static HELLO_WORLD: ArcSlice<str, ArcLayout<true, true>> =
    ///     ArcSlice::<str, ArcLayout<true, true>>::from_static("hello world");
    /// ```
    pub const fn from_static(slice: &'static str) -> Self {
        // MSRV 1.65 const `<*const _>::cast_mut` + 1.85 const `NonNull::new`
        let start = unsafe { NonNull::new_unchecked(slice.as_ptr() as _) };
        let length = slice.len();
        let data = unsafe { L::STATIC_DATA_UNCHECKED.assume_init() };
        Self::init(start, length, data)
    }
}

impl<S: Slice + ?Sized, L: Layout> Drop for ArcSlice<S, L> {
    fn drop(&mut self) {
        unsafe { L::drop::<S, false>(self.start, self.length, &mut self.data) };
    }
}

impl<
        S: Slice + ?Sized,
        #[cfg(feature = "oom-handling")] L: Layout,
        #[cfg(not(feature = "oom-handling"))] L: CloneNoAllocLayout,
    > Clone for ArcSlice<S, L>
{
    fn clone(&self) -> Self {
        self.clone_impl::<Infallible>().unwrap_checked()
    }
}

impl<S: Slice + ?Sized, L: Layout> Deref for ArcSlice<S, L> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<S: Slice + ?Sized, L: Layout> AsRef<S> for ArcSlice<S, L> {
    fn as_ref(&self) -> &S {
        self
    }
}

impl<S: Hash + Slice + ?Sized, L: Layout> Hash for ArcSlice<S, L> {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<S: Slice + ?Sized, L: Layout> Borrow<S> for ArcSlice<S, L> {
    fn borrow(&self) -> &S {
        self
    }
}

impl<S: Emptyable + ?Sized, L: StaticLayout> Default for ArcSlice<S, L> {
    fn default() -> Self {
        Self::new_empty(NonNull::dangling(), 0).unwrap_checked()
    }
}

impl<S: fmt::Debug + Slice + ?Sized, L: Layout> fmt::Debug for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self.as_slice(), f)
    }
}

impl<S: fmt::Display + Slice + ?Sized, L: Layout> fmt::Display for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::LowerHex for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self.to_slice(), f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::UpperHex for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self.to_slice(), f)
    }
}

impl<S: PartialEq + Slice + ?Sized, L: Layout> PartialEq for ArcSlice<S, L> {
    fn eq(&self, other: &ArcSlice<S, L>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<S: PartialEq + Slice + ?Sized, L: Layout> Eq for ArcSlice<S, L> {}

impl<S: PartialOrd + Slice + ?Sized, L: Layout> PartialOrd for ArcSlice<S, L> {
    fn partial_cmp(&self, other: &ArcSlice<S, L>) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<S: Ord + Slice + ?Sized, L: Layout> Ord for ArcSlice<S, L> {
    fn cmp(&self, other: &ArcSlice<S, L>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<S: PartialEq + Slice + ?Sized, L: Layout> PartialEq<S> for ArcSlice<S, L> {
    fn eq(&self, other: &S) -> bool {
        self.as_slice() == other
    }
}

impl<'a, S: PartialEq + Slice + ?Sized, L: Layout> PartialEq<&'a S> for ArcSlice<S, L> {
    fn eq(&self, other: &&'a S) -> bool {
        self.as_slice() == *other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<[T; N]>
    for ArcSlice<[T], L>
{
    fn eq(&self, other: &[T; N]) -> bool {
        *other == **self
    }
}

impl<'a, T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<&'a [T; N]>
    for ArcSlice<[T], L>
{
    fn eq(&self, other: &&'a [T; N]) -> bool {
        **other == **self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<ArcSlice<[T], L>>
    for [T; N]
{
    fn eq(&self, other: &ArcSlice<[T], L>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<ArcSlice<[T], L>> for [T] {
    fn eq(&self, other: &ArcSlice<[T], L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<ArcSlice<str, L>> for str {
    fn eq(&self, other: &ArcSlice<str, L>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<Vec<T>> for ArcSlice<[T], L> {
    fn eq(&self, other: &Vec<T>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<String> for ArcSlice<str, L> {
    fn eq(&self, other: &String) -> bool {
        **self == **other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<ArcSlice<[T], L>> for Vec<T> {
    fn eq(&self, other: &ArcSlice<[T], L>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<ArcSlice<str, L>> for String {
    fn eq(&self, other: &ArcSlice<str, L>) -> bool {
        **self == **other
    }
}

#[cfg(feature = "oom-handling")]
impl<S: Slice + ?Sized, L: Layout> From<&S> for ArcSlice<S, L>
where
    S::Item: Copy,
{
    fn from(value: &S) -> Self {
        Self::from_slice(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<T: Copy + Send + Sync + 'static, L: Layout, const N: usize> From<&[T; N]>
    for ArcSlice<[T], L>
{
    fn from(value: &[T; N]) -> Self {
        Self::from_slice(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<T: Send + Sync + 'static, L: Layout, const N: usize> From<[T; N]> for ArcSlice<[T], L> {
    fn from(value: [T; N]) -> Self {
        Self::from_array(value)
    }
}

#[cfg(not(feature = "oom-handling"))]
impl<S: Slice + ?Sized> From<Box<S>> for ArcSlice<S, BoxedSliceLayout> {
    fn from(value: Box<S>) -> Self {
        Self::from_vec(unsafe { S::from_vec_unchecked(value.into_boxed_slice().into_vec()) })
    }
}
#[cfg(not(feature = "oom-handling"))]
impl<S: Slice + ?Sized> From<Box<S>> for ArcSlice<S, VecLayout> {
    fn from(value: Box<S>) -> Self {
        Self::from_vec(unsafe { S::from_vec_unchecked(value.into_boxed_slice().into_vec()) })
    }
}
#[cfg(feature = "oom-handling")]
impl<S: Slice + ?Sized, L: AnyBufferLayout> From<Box<S>> for ArcSlice<S, L> {
    fn from(value: Box<S>) -> Self {
        Self::from_vec(unsafe { S::from_vec_unchecked(value.into_boxed_slice().into_vec()) })
    }
}

#[cfg(not(feature = "oom-handling"))]
impl<T: Send + Sync + 'static> From<Vec<T>> for ArcSlice<[T], VecLayout> {
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}
#[cfg(feature = "oom-handling")]
impl<T: Send + Sync + 'static, L: AnyBufferLayout> From<Vec<T>> for ArcSlice<[T], L> {
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}

#[cfg(not(feature = "oom-handling"))]
impl From<String> for ArcSlice<str, crate::layout::VecLayout> {
    fn from(value: String) -> Self {
        Self::from_vec(value)
    }
}
#[cfg(feature = "oom-handling")]
impl<L: AnyBufferLayout> From<String> for ArcSlice<str, L> {
    fn from(value: String) -> Self {
        Self::from_vec(value)
    }
}

impl<T: Send + Sync + 'static, L: Layout, const N: usize> TryFrom<ArcSlice<[T], L>> for [T; N] {
    type Error = ArcSlice<[T], L>;
    fn try_from(value: ArcSlice<[T], L>) -> Result<Self, Self::Error> {
        let mut this = ManuallyDrop::new(value);
        unsafe { L::take_array::<T, N>(this.start, this.length, &mut this.data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }
}

#[cfg(feature = "oom-handling")]
impl<L: Layout> core::str::FromStr for ArcSlice<str, L> {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.into())
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;

    impl<L: Layout> std::io::Read for ArcSlice<[u8], L> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = cmp::min(self.len(), buf.len());
            buf[..n].copy_from_slice(&self[..n]);
            Ok(n)
        }
    }
};

/// TODO
pub struct ArcSliceBorrow<'a, S: Slice + ?Sized, L: Layout = DefaultLayout> {
    start: NonNull<S::Item>,
    length: usize,
    ptr: *const (),
    _phantom: PhantomData<&'a ArcSlice<S, L>>,
}

unsafe impl<S: Slice + ?Sized, L: Layout> Send for ArcSliceBorrow<'_, S, L> {}
unsafe impl<S: Slice + ?Sized, L: Layout> Sync for ArcSliceBorrow<'_, S, L> {}

impl<S: Slice + ?Sized, L: Layout> Clone for ArcSliceBorrow<'_, S, L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: Slice + ?Sized, L: Layout> Copy for ArcSliceBorrow<'_, S, L> {}

impl<S: Slice + ?Sized, L: Layout> Deref for ArcSliceBorrow<'_, S, L> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<S: fmt::Debug + Slice + ?Sized, L: Layout> fmt::Debug for ArcSliceBorrow<'_, S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(&**self, f)
    }
}

impl<'a, S: Slice + ?Sized, L: Layout> ArcSliceBorrow<'a, S, L> {
    #[allow(clippy::wrong_self_convention)]
    fn clone_arc_impl<E: AllocErrorImpl>(self) -> Result<ArcSlice<S, L>, E> {
        if let Some(empty) = ArcSlice::new_empty(self.start, self.length) {
            return Ok(empty);
        }
        let clone = || {
            let arc_slice = unsafe { &*self.ptr.cast::<ArcSlice<S, L>>() };
            L::clone::<S, E>(arc_slice.start, arc_slice.length, &arc_slice.data)
        };
        let data = L::clone_borrowed_data::<S>(self.ptr).map_or_else(clone, Ok)?;
        Ok(ArcSlice {
            start: self.start,
            length: self.length,
            data: ManuallyDrop::new(data),
        })
    }

    /// Tries cloning the borrow into a subslice of the underlying [`ArcSlice`], returning an
    /// error if an allocation fails.
    ///
    /// The returned [`ArcSlice`] has the same slice as the original borrow.
    ///
    /// The operation may not allocate, see
    /// [`CloneNoAllocLayout`](crate::layout::CloneNoAllocLayout) documentation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let borrow = s.borrow(..5);
    /// assert_eq!(&borrow[..], b"hello");
    /// let s2: ArcSlice<[u8]> = borrow.try_clone_arc().unwrap();
    /// assert_eq!(s2, b"hello");
    /// ```
    pub fn try_clone_arc(self) -> Result<ArcSlice<S, L>, AllocError> {
        self.clone_arc_impl::<AllocError>()
    }

    /// Extracts the borrowed slice.
    ///
    /// Roughly equivalent to `&self[..]`, but using the borrow lifetime instead of self's one.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// assert_eq!(s.as_slice(), b"hello world");
    /// ```
    pub fn as_slice(&self) -> &'a S {
        unsafe { S::from_raw_parts(self.start, self.length) }
    }

    /// Reborrows a subslice of an `ArcSliceBorrow` with a given range.
    ///
    /// The range is applied to the `ArcSliceBorrow` slice, not to the underlying `ArcSlice` one.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let borrow = s.borrow(..5);
    /// assert_eq!(&borrow[..], b"hello");
    /// let reborrow = borrow.reborrow(2..4);
    /// assert_eq!(&reborrow[..], b"ll");
    /// ```
    pub fn reborrow(&self, range: impl RangeBounds<usize>) -> ArcSliceBorrow<'a, S, L>
    where
        S: Subsliceable,
    {
        unsafe { self.reborrow_impl(range_offset_len(self.as_slice(), range)) }
    }

    /// Reborrows a subslice of an `ArcSliceBorrow` from a slice reference.
    ///
    /// The slice reference must be contained into the `ArcSliceBorrow` slice, not into the underlying `ArcSlice` one.
    ///
    /// # Examples
    ///
    ///```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let hello = &s[..5];
    /// let borrow = s.borrow_from_ref(hello);
    /// assert_eq!(&borrow[..], b"hello");
    /// let ll = &borrow[2..4];
    /// let reborrow = borrow.reborrow_from_ref(ll);
    /// assert_eq!(&reborrow[..], b"ll");
    /// ```
    pub fn reborrow_from_ref(&self, subset: &S) -> ArcSliceBorrow<'a, S, L>
    where
        S: Subsliceable,
    {
        unsafe { self.reborrow_impl(subslice_offset_len(self.as_slice(), subset)) }
    }

    unsafe fn reborrow_impl(&self, (offset, len): (usize, usize)) -> ArcSliceBorrow<'a, S, L>
    where
        S: Subsliceable,
    {
        ArcSliceBorrow {
            start: unsafe { self.start.add(offset) },
            length: len,
            ptr: self.ptr,
            _phantom: PhantomData,
        }
    }
}

impl<
        S: Slice + ?Sized,
        #[cfg(feature = "oom-handling")] L: Layout,
        #[cfg(not(feature = "oom-handling"))] L: CloneNoAllocLayout,
    > ArcSliceBorrow<'_, S, L>
{
    /// Clone the borrow into a subslice of the underlying [`ArcSlice`].
    ///
    /// The returned [`ArcSlice`] has the same slice as the original borrow.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_slice::ArcSlice;
    ///
    /// let s = ArcSlice::<[u8]>::from(b"hello world");
    /// let borrow = s.borrow(..5);
    /// assert_eq!(&borrow[..], b"hello");
    /// let s2: ArcSlice<[u8]> = borrow.clone_arc();
    /// assert_eq!(s2, b"hello");
    /// ```
    pub fn clone_arc(self) -> ArcSlice<S, L> {
        self.clone_arc_impl::<Infallible>().unwrap_checked()
    }
}
