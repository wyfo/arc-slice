//! The slice and buffer traits used by [`ArcSlice`] and [`ArcSliceMut`].
//!
//! [`ArcSlice`]: crate::ArcSlice
//! [`ArcSliceMut`]: crate::ArcSliceMut
use alloc::{alloc::realloc, boxed::Box, string::String, vec::Vec};
use core::{
    alloc::{Layout, LayoutError},
    any::Any,
    cmp::max,
    convert::Infallible,
    mem,
    mem::ManuallyDrop,
    ptr,
    ptr::{addr_of, addr_of_mut, NonNull},
    slice,
};

pub(crate) use crate::buffer::private::DynBuffer;
#[allow(unused_imports)]
use crate::msrv::{ConstPtrExt, NonNullExt, SlicePtrExt};
use crate::{
    error::TryReserveError,
    macros::assume,
    msrv::SubPtrExt,
    slice_mut::TryReserveResult,
    utils::{assert_checked, NewChecked},
};

/// A slice, e.g. `[T]` or `str`.
///
/// # Safety
///
/// - [`into_vec`](Self::into_vec) must be pure, i.e. `mem::forget(S::into_vec(ptr::read(vec_ptr)))`
///   should not invalidate memory behind `vec_ptr`.
/// - If [`try_from_slice_mut`](Self::try_from_slice_mut) returns `Ok`, then
///   [`try_from_slice`](Self::try_from_slice) must also return `Ok` for the same slice.
pub unsafe trait Slice: Send + Sync + 'static {
    /// The slice item, e.g. `T` for `[T]` or `u8` for `str`.
    type Item: Send + Sync + 'static;
    /// The associated vector to the slice type, e.g. `Vec<T>` for `[T]` or `String` for `str`.
    type Vec: BufferMut<Self>;

    /// Convert a slice to its underlying item slice.
    fn to_slice(&self) -> &[Self::Item];
    /// Convert a mutable slice to its underlying item slice.
    ///
    /// # Safety
    ///
    /// The item slice is never mutated, as it is only used for storage.
    unsafe fn to_slice_mut(&mut self) -> &mut [Self::Item];
    /// Convert a boxed slice to its underlying boxed item slice.
    fn into_boxed_slice(self: Box<Self>) -> Box<[Self::Item]>;
    /// Convert a vector to its underlying item vector.
    fn into_vec(vec: Self::Vec) -> Vec<Self::Item>;

    /// Convert back a slice from its underlying item slice.
    ///
    /// # Safety
    ///
    /// The item slice must have been obtained from [`Self::to_slice`].
    unsafe fn from_slice_unchecked(slice: &[Self::Item]) -> &Self;
    /// Convert back a mutable slice from its underlying item slice.
    ///
    /// # Safety
    ///
    /// The item slice must have been obtained from [`Self::to_slice_mut`].
    unsafe fn from_slice_mut_unchecked(slice: &mut [Self::Item]) -> &mut Self;
    /// Convert back a boxed slice from its underlying boxed item slice.
    ///
    /// # Safety
    ///
    /// The boxed item slice must have been obtained from [`Self::into_boxed_slice`].
    unsafe fn from_boxed_slice_unchecked(boxed: Box<[Self::Item]>) -> Box<Self>;
    /// Convert back a vector from its underlying item vector.
    ///
    /// # Safety
    ///
    /// The boxed item slice must have been obtained from [`Self::into_vec`].
    unsafe fn from_vec_unchecked(vec: Vec<Self::Item>) -> Self::Vec;

    /// Error which can occur when attempting to convert an item slice to the given slice type.
    type TryFromSliceError;
    /// Try converting an item slice to the given slice type.
    fn try_from_slice(slice: &[Self::Item]) -> Result<&Self, Self::TryFromSliceError>;
    /// Try converting a mutable item slice to the given slice type.
    fn try_from_slice_mut(slice: &mut [Self::Item]) -> Result<&mut Self, Self::TryFromSliceError>;
}

pub(crate) trait SliceExt: Slice {
    fn as_ptr(&self) -> NonNull<Self::Item> {
        NonNull::new_checked(self.to_slice().as_ptr().cast_mut())
    }
    fn as_mut_ptr(&mut self) -> NonNull<Self::Item> {
        NonNull::new_checked(unsafe { self.to_slice_mut().as_mut_ptr() })
    }
    fn len(&self) -> usize {
        self.to_slice().len()
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn to_raw_parts(&self) -> (NonNull<Self::Item>, usize) {
        (self.as_ptr(), self.len())
    }
    fn to_raw_parts_mut(&mut self) -> (NonNull<Self::Item>, usize) {
        (self.as_mut_ptr(), self.len())
    }
    unsafe fn from_raw_parts<'a>(start: NonNull<Self::Item>, length: usize) -> &'a Self {
        unsafe { Self::from_slice_unchecked(slice::from_raw_parts(start.as_ptr(), length)) }
    }
    unsafe fn from_raw_parts_mut<'a>(start: NonNull<Self::Item>, length: usize) -> &'a mut Self {
        unsafe { Self::from_slice_mut_unchecked(slice::from_raw_parts_mut(start.as_ptr(), length)) }
    }
    // use this instead of `BufferMutExt::as_mut_ptr` as the pointer
    // is not invalidated when the vector is moved
    fn vec_start(vec: &mut Self::Vec) -> NonNull<Self::Item> {
        let mut vec = ManuallyDrop::new(Self::into_vec(unsafe { ptr::read(vec) }));
        NonNull::new_checked(vec.as_mut_ptr())
    }
    fn needs_drop() -> bool {
        mem::needs_drop::<Self::Item>()
    }
}

impl<S: Slice + ?Sized> SliceExt for S {}

/// A slice that can be empty.
///
/// # Safety
///
/// `Slice::try_from_slice(&[])`/`Slice::try_from_slice_mut(&mut [])` must be ok.
pub unsafe trait Emptyable: Slice {}

/// A slice that can be safely initialized from an all-zero byte-pattern.
///
/// # Safety
///
/// An item slice allocated with [`alloc_zeroed`](alloc::alloc::alloc_zeroed) must be convertible
/// to the given slice.
pub unsafe trait Zeroable: Slice {}

/// A slice that can be split into smaller subslices.
///
/// # Safety
///
/// If [`Self::check_subslice`] (or other derived methods) doesn't panic, then the subslice
/// with the given range must be valid.
pub unsafe trait Subsliceable: Slice {
    /// Check if a subslice is valid.
    ///
    /// # Safety
    ///
    /// `start..end` must be a valid range of the item slice returned by [`Slice::to_slice`].
    unsafe fn check_subslice(&self, start: usize, end: usize);
    /// Same as `self.check_subslice(offset, self.to_slice().len())`.
    ///
    /// # Safety
    ///
    /// See [`Self::check_subslice`].
    unsafe fn check_advance(&self, offset: usize) {
        unsafe { self.check_subslice(offset, self.len()) }
    }
    /// Same as `self.check_subslice(0, offset))`.
    ///
    /// # Safety
    ///
    /// See [`Self::check_subslice`].
    unsafe fn check_truncate(&self, len: usize) {
        unsafe { self.check_subslice(0, len) }
    }
    /// Same as `{ self.check_subslice(0, at)); self.check_subslice(at, self.to_slice().len()) }`.
    ///
    /// # Safety
    ///
    /// See [`Self::check_subslice`].
    unsafe fn check_split(&self, at: usize) {
        unsafe { self.check_subslice(0, at) };
        unsafe { self.check_subslice(at, self.len()) };
    }
}

/// A slice that can be concatenated.
///
/// # Safety
///
/// The concatenation of two slices must be a valid slice.
pub unsafe trait Concatenable: Slice {}

/// A slice that can be extended with arbitrary items.
///
/// # Safety
///
/// The concatenation of a slice with an additional item must be a valid slice.
pub unsafe trait Extendable: Concatenable {}

/// A slice that can be deserialized according to the [`serde` data model]
///
/// [`serde` data model]: https://serde.rs/data-model.html
#[cfg(feature = "serde")]
pub trait Deserializable: Slice
where
    Self::Item: for<'a> serde::Deserialize<'a>,
    Self::TryFromSliceError: core::fmt::Display,
{
    /// Deserialize a slice with the given visitor.
    fn deserialize<'de, D: serde::Deserializer<'de>, V: serde::de::Visitor<'de>>(
        deserializer: D,
        visitor: V,
    ) -> Result<V::Value, D::Error>;
    /// What data the visitor expects to receive.
    fn expecting(f: &mut core::fmt::Formatter) -> core::fmt::Result;
    /// Deserialize a slice from bytes.
    fn deserialize_from_bytes<E: serde::de::Error>(bytes: &[u8]) -> Result<&Self, E>;
    /// Deserialize a vector from owned bytes.
    fn deserialize_from_byte_buf<E: serde::de::Error>(bytes: Vec<u8>) -> Result<Self::Vec, E>;
    /// Deserialize a slice from string.
    fn deserialize_from_str<E: serde::de::Error>(s: &str) -> Result<&Self, E>;
    /// Deserialize a slice from owned string.
    fn deserialize_from_string<E: serde::de::Error>(s: String) -> Result<Self::Vec, E>;
    /// Try deserializing a slice from a sequence.
    ///
    /// The sequence will be collected into an `ArcSliceMut<[S::Item]>` before calling
    /// [`ArcSliceMut::try_from_arc_slice_mut`](crate::ArcSliceMut::try_from_arc_slice_mut).
    fn try_deserialize_from_seq() -> bool;
}

unsafe impl<T: Send + Sync + 'static> Slice for [T] {
    type Item = T;
    type Vec = Vec<T>;

    fn to_slice(&self) -> &[Self::Item] {
        self
    }
    unsafe fn to_slice_mut(&mut self) -> &mut [Self::Item] {
        self
    }
    fn into_boxed_slice(self: Box<Self>) -> Box<[Self::Item]> {
        self
    }
    fn into_vec(vec: Self::Vec) -> Vec<Self::Item> {
        vec
    }

    unsafe fn from_slice_unchecked(slice: &[Self::Item]) -> &Self {
        slice
    }
    unsafe fn from_slice_mut_unchecked(slice: &mut [Self::Item]) -> &mut Self {
        slice
    }
    unsafe fn from_boxed_slice_unchecked(boxed: Box<[Self::Item]>) -> Box<Self> {
        boxed
    }
    unsafe fn from_vec_unchecked(vec: Vec<Self::Item>) -> Self::Vec {
        vec
    }

    type TryFromSliceError = Infallible;
    fn try_from_slice(slice: &[Self::Item]) -> Result<&Self, Self::TryFromSliceError> {
        Ok(slice)
    }
    fn try_from_slice_mut(slice: &mut [Self::Item]) -> Result<&mut Self, Self::TryFromSliceError> {
        Ok(slice)
    }
}

unsafe impl<T: Send + Sync + 'static> Emptyable for [T] {}
unsafe impl Emptyable for str {}

#[cfg(feature = "bytemuck")]
unsafe impl<T: bytemuck::Zeroable + Send + Sync + 'static> Zeroable for [T] {}
#[cfg(not(feature = "bytemuck"))]
unsafe impl Zeroable for [u8] {}
unsafe impl Zeroable for str {}

unsafe impl<T: Send + Sync + 'static> Subsliceable for [T] {
    unsafe fn check_subslice(&self, _start: usize, _end: usize) {}
}

unsafe impl<T: Send + Sync + 'static> Concatenable for [T] {}

unsafe impl<T: Send + Sync + 'static> Extendable for [T] {}

#[cfg(feature = "serde")]
fn invalid_type<T: for<'a> serde::Deserialize<'a> + Send + Sync + 'static, E: serde::de::Error>(
    unexpected: serde::de::Unexpected,
) -> E {
    struct Expected<T>(core::marker::PhantomData<T>);
    impl<T: for<'a> serde::Deserialize<'a> + Send + Sync + 'static> serde::de::Expected
        for Expected<T>
    {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            <[T]>::expecting(f)
        }
    }
    E::invalid_type(unexpected, &Expected(core::marker::PhantomData::<T>))
}

#[cfg(feature = "serde")]
impl<T: for<'a> serde::Deserialize<'a> + Send + Sync + 'static> Deserializable for [T] {
    fn deserialize<'de, D: serde::Deserializer<'de>, V: serde::de::Visitor<'de>>(
        deserializer: D,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        if crate::macros::is!(T, u8) {
            deserializer.deserialize_byte_buf(visitor)
        } else {
            deserializer.deserialize_seq(visitor)
        }
    }
    fn expecting(f: &mut core::fmt::Formatter) -> core::fmt::Result {
        if crate::macros::is!(T, u8) {
            write!(f, "a byte string")
        } else {
            write!(f, "a sequence")
        }
    }
    fn deserialize_from_bytes<E: serde::de::Error>(bytes: &[u8]) -> Result<&Self, E> {
        if crate::macros::is!(T, u8) {
            Ok(unsafe { bytes.align_to().1 })
        } else {
            Err(invalid_type::<T, E>(serde::de::Unexpected::Bytes(bytes)))
        }
    }
    fn deserialize_from_byte_buf<E: serde::de::Error>(bytes: Vec<u8>) -> Result<Self::Vec, E> {
        crate::utils::try_transmute(bytes)
            .map_err(|bytes| invalid_type::<T, E>(serde::de::Unexpected::Bytes(&bytes)))
    }
    fn deserialize_from_str<E: serde::de::Error>(s: &str) -> Result<&Self, E> {
        Err(invalid_type::<T, E>(serde::de::Unexpected::Str(s)))
    }
    fn deserialize_from_string<E: serde::de::Error>(s: String) -> Result<Self::Vec, E> {
        Err(invalid_type::<T, E>(serde::de::Unexpected::Str(&s)))
    }
    fn try_deserialize_from_seq() -> bool {
        crate::macros::is_not!(T, u8)
    }
}

unsafe impl Slice for str {
    type Item = u8;
    type Vec = String;

    fn to_slice(&self) -> &[Self::Item] {
        self.as_bytes()
    }
    unsafe fn to_slice_mut(&mut self) -> &mut [Self::Item] {
        unsafe { self.as_bytes_mut() }
    }
    fn into_boxed_slice(self: Box<Self>) -> Box<[Self::Item]> {
        self.into_boxed_bytes()
    }
    fn into_vec(vec: Self::Vec) -> Vec<Self::Item> {
        vec.into_bytes()
    }

    unsafe fn from_slice_unchecked(slice: &[Self::Item]) -> &Self {
        unsafe { core::str::from_utf8_unchecked(slice) }
    }
    unsafe fn from_slice_mut_unchecked(slice: &mut [Self::Item]) -> &mut Self {
        unsafe { core::str::from_utf8_unchecked_mut(slice) }
    }
    unsafe fn from_boxed_slice_unchecked(boxed: Box<[Self::Item]>) -> Box<Self> {
        unsafe { alloc::str::from_boxed_utf8_unchecked(boxed) }
    }
    unsafe fn from_vec_unchecked(vec: Vec<Self::Item>) -> Self::Vec {
        unsafe { String::from_utf8_unchecked(vec) }
    }

    type TryFromSliceError = core::str::Utf8Error;
    fn try_from_slice(slice: &[Self::Item]) -> Result<&Self, Self::TryFromSliceError> {
        core::str::from_utf8(slice)
    }
    fn try_from_slice_mut(slice: &mut [Self::Item]) -> Result<&mut Self, Self::TryFromSliceError> {
        core::str::from_utf8_mut(slice)
    }
}

pub(crate) fn check_char_boundary(s: &str, offset: usize) {
    #[cold]
    fn panic_not_a_char_boundary() -> ! {
        panic!("not a char boundary")
    }
    unsafe { assume!(offset <= s.len()) };
    if !s.is_char_boundary(offset) {
        panic_not_a_char_boundary();
    }
}

unsafe impl Subsliceable for str {
    unsafe fn check_subslice(&self, start: usize, end: usize) {
        check_char_boundary(self, start);
        check_char_boundary(self, end);
    }

    unsafe fn check_split(&self, at: usize) {
        check_char_boundary(self, at);
    }
}

unsafe impl Concatenable for str {}

#[cfg(feature = "serde")]
impl Deserializable for str {
    fn deserialize<'de, D: serde::Deserializer<'de>, V: serde::de::Visitor<'de>>(
        deserializer: D,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        deserializer.deserialize_string(visitor)
    }
    fn expecting(f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "a string")
    }
    fn deserialize_from_bytes<E: serde::de::Error>(bytes: &[u8]) -> Result<&Self, E> {
        core::str::from_utf8(bytes).map_err(E::custom)
    }
    fn deserialize_from_byte_buf<E: serde::de::Error>(bytes: Vec<u8>) -> Result<Self::Vec, E> {
        String::from_utf8(bytes).map_err(E::custom)
    }
    fn deserialize_from_str<E: serde::de::Error>(s: &str) -> Result<&Self, E> {
        Ok(s)
    }
    fn deserialize_from_string<E: serde::de::Error>(s: String) -> Result<Self::Vec, E> {
        Ok(s)
    }
    fn try_deserialize_from_seq() -> bool {
        false
    }
}

/// A buffer that contains a slice.
pub trait Buffer<S: ?Sized>: Sized + Send + 'static {
    /// Returns the buffer slice.
    fn as_slice(&self) -> &S;
    /// Returns if the buffer is unique, i.e. if this buffer is the only reference to its slice.
    fn is_unique(&self) -> bool {
        true
    }
}

impl<S: Slice + ?Sized> Buffer<S> for &'static S {
    fn as_slice(&self) -> &S {
        self
    }

    fn is_unique(&self) -> bool {
        false
    }
}

impl<S: Slice + ?Sized> Buffer<S> for Box<S> {
    fn as_slice(&self) -> &S {
        self
    }
}

impl<T: Send + Sync + 'static> Buffer<[T]> for Vec<T> {
    fn as_slice(&self) -> &[T] {
        self
    }
}

impl Buffer<str> for String {
    fn as_slice(&self) -> &str {
        self
    }
}

pub(crate) trait BufferExt<S: Slice + ?Sized>: Buffer<S> {
    #[allow(unstable_name_collisions)]
    unsafe fn offset(&self, start: NonNull<S::Item>) -> usize {
        unsafe { start.sub_ptr(self.as_slice().as_ptr()) }
    }

    fn len(&self) -> usize {
        self.as_slice().to_raw_parts().1
    }
}

impl<S: Slice + ?Sized, B: Buffer<S>> BufferExt<S> for B {}

/// A buffer that contains a mutable slice.
///
/// The buffer may be resizable, and the whole slice may have an uninitialized section.
///
/// # Safety
///
/// - [`as_mut_slice`] must return the same slice as [`Buffer::as_slice`]
/// - The full buffer slice must have at least [`capacity`] maybe uninitialized items;
///   [`as_mut_slice`] returns in fact the beginning of the full slice.
/// - Accessing [`capacity`] must not invalidate the buffer slice.
/// - If [`set_len`] returns `true`, then the length of [`as_mut_slice`] must have been
///   updated accordingly.
/// - If [`try_reserve`] returns successfully, then [`capacity`] must have been increased
///   by at least `additional` items.
/// - If the buffer implements [`BorrowMetadata`], then [`borrow_metadata`] must not
///   invalidate the buffer slice.
///
/// [`as_mut_slice`]: Self::as_mut_slice
/// [`capacity`]: Self::capacity
/// [`set_len`]: Self::set_len
/// [`try_reserve`]: Self::try_reserve
/// [`borrow_metadata`]: BorrowMetadata::borrow_metadata
#[allow(clippy::len_without_is_empty)]
pub unsafe trait BufferMut<S: ?Sized>: Buffer<S> + Sync {
    /// Returns the mutable buffer slice.
    fn as_mut_slice(&mut self) -> &mut S;
    /// Returns the buffer capacity.
    fn capacity(&self) -> usize;
    /// Set the length of the buffer slice.
    ///
    /// # Safety
    ///
    /// First `len` items of buffer slice must be initialized.
    unsafe fn set_len(&mut self, len: usize) -> bool;
    /// Try reserving capacity for at least `additional` items.
    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError>;
}

unsafe impl<T: Send + Sync + 'static> BufferMut<[T]> for Vec<T> {
    fn as_mut_slice(&mut self) -> &mut [T] {
        self
    }

    fn capacity(&self) -> usize {
        self.capacity()
    }

    unsafe fn set_len(&mut self, len: usize) -> bool {
        // SAFETY: same function contract
        unsafe { self.set_len(len) };
        true
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        let overflow = |len| (len as isize).checked_add(additional as isize).is_none();
        match self.try_reserve(additional) {
            Ok(()) => Ok(()),
            Err(_) if overflow(self.len()) => Err(TryReserveError::CapacityOverflow),
            Err(_) => Err(TryReserveError::AllocError),
        }
    }
}

unsafe impl BufferMut<str> for String {
    fn as_mut_slice(&mut self) -> &mut str {
        self
    }

    fn capacity(&self) -> usize {
        self.capacity()
    }

    unsafe fn set_len(&mut self, len: usize) -> bool {
        // SAFETY: same function contract
        unsafe { self.as_mut_vec().set_len(len) };
        true
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        BufferMut::try_reserve(unsafe { self.as_mut_vec() }, additional)
    }
}

pub(crate) trait BufferMutExt<S: Slice + ?Sized>: BufferMut<S> {
    unsafe fn realloc<T>(
        &mut self,
        additional: usize,
        ptr: NonNull<T>,
        layout: impl Fn(usize) -> Result<Layout, LayoutError>,
    ) -> Result<(NonNull<T>, usize), TryReserveError> {
        let required = self
            .len()
            .checked_add(additional)
            .ok_or(TryReserveError::CapacityOverflow)?;
        let new_capacity = max(self.capacity() * 2, required);
        let cur_layout = unsafe { layout(self.capacity()).unwrap_unchecked() };
        let new_layout = layout(new_capacity).map_err(|_| TryReserveError::CapacityOverflow)?;
        let new_ptr =
            NonNull::new(unsafe { realloc(ptr.as_ptr().cast(), cur_layout, new_layout.size()) })
                .ok_or(TryReserveError::AllocError)?;
        Ok((new_ptr.cast(), new_capacity))
    }

    unsafe fn shift_left(
        &mut self,
        offset: usize,
        length: usize,
        // do not use the pointer derived from slice as it is invalidated with the slice
        start: impl Fn(&mut Self) -> NonNull<S::Item>,
    ) -> bool {
        assert_checked(!mem::needs_drop::<S::Item>());
        let prev_len = self.len();
        if length == prev_len {
            return true;
        }
        if !unsafe { self.set_len(length) } {
            return false;
        }
        let src = unsafe { start(self).add(offset) }.as_ptr();
        let dst = start(self).as_ptr();
        if offset == 0 {
            return true;
        } else if offset >= length {
            unsafe { ptr::copy_nonoverlapping(src, dst, length) };
        } else {
            unsafe { ptr::copy(src, dst, length) };
        }
        true
    }

    unsafe fn try_reserve_impl(
        &mut self,
        offset: usize,
        length: usize,
        additional: usize,
        allocate: bool,
        // do not use the pointer derived from slice as it is invalidated with the slice
        start: impl Fn(&mut Self) -> NonNull<S::Item>,
    ) -> TryReserveResult<S::Item> {
        let capacity = self.capacity();
        if capacity - offset - length >= additional {
            return (Ok(capacity - offset), unsafe { start(self).add(offset) });
        }
        if !mem::needs_drop::<S::Item>()
            // conditions from `BytesMut::reserve_inner`
            && self.capacity() - length >= additional
            && offset >= length
            && unsafe { self.shift_left(offset, length, &start) }
        {
            return (Ok(capacity), start(self));
        }
        if allocate && unsafe { self.set_len(offset + length) } {
            let capacity = self
                .try_reserve(additional)
                .map(|_| self.capacity() - offset);
            return (capacity, unsafe { start(self).add(offset) });
        }
        (Err(TryReserveError::Unsupported), unsafe {
            start(self).add(offset)
        })
    }
}

impl<S: Slice + ?Sized, B: BufferMut<S>> BufferMutExt<S> for B {}

#[cfg(feature = "raw-buffer")]
/// A buffer that can be stored into a raw pointer.
///
/// The trait can be used when the actual buffer is already stored in an [`Arc`].
///
/// # Safety
///
/// - The slice returned by [`Buffer::as_slice`] must not be invalidated by
///   [`into_raw`].
/// - [`from_raw`] must be pure, i.e. `mem::forget(S::from_raw(ptr))` should not
///   invalidate memory behind ptr.
///
/// [`Arc`]: alloc::sync::Arc
/// [`into_raw`]: Self::into_raw
/// [`from_raw`]: Self::from_raw
pub unsafe trait RawBuffer<S: ?Sized>: Buffer<S> + Clone {
    fn into_raw(self) -> *const ();
    /// # Safety
    /// The pointer must be obtained by a call to [`RawBuffer::into_raw`].
    unsafe fn from_raw(ptr: *const ()) -> Self;
}

/// A trait for borrowing metadata.
pub trait BorrowMetadata: Sync {
    /// The metadata borrowed.
    type Metadata: Sync + 'static;
    /// Borrow the metadata.
    fn borrow_metadata(&self) -> &Self::Metadata;
}

unsafe impl<B: BorrowMetadata + Any> DynBuffer for B {
    type Buffer = B;
    type Metadata = B::Metadata;

    fn get_metadata(&self) -> &Self::Metadata {
        self.borrow_metadata()
    }

    unsafe fn take_buffer(this: *mut Self, buffer: NonNull<()>) {
        unsafe { ptr::copy_nonoverlapping(this, buffer.as_ptr().cast(), 1) }
    }
}

#[derive(Clone)]
pub(crate) struct BufferWithMetadata<B, M> {
    buffer: B,
    metadata: M,
}

impl<B, M> BufferWithMetadata<B, M> {
    pub(crate) fn new(buffer: B, metadata: M) -> Self {
        Self { buffer, metadata }
    }

    pub(crate) fn buffer(self) -> B {
        self.buffer
    }

    pub(crate) fn into_tuple(self) -> (B, M) {
        (self.buffer, self.metadata)
    }
}

impl<S: Slice + ?Sized, B: Buffer<S>, M: Send + Sync + 'static> Buffer<S>
    for BufferWithMetadata<B, M>
{
    fn as_slice(&self) -> &S {
        self.buffer.as_slice()
    }

    fn is_unique(&self) -> bool {
        self.buffer.is_unique()
    }
}

unsafe impl<S: Slice + ?Sized, B: BufferMut<S>, M: Send + Sync + 'static> BufferMut<S>
    for BufferWithMetadata<B, M>
{
    fn as_mut_slice(&mut self) -> &mut S {
        self.buffer.as_mut_slice()
    }

    fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    unsafe fn set_len(&mut self, len: usize) -> bool {
        unsafe { self.buffer.set_len(len) }
    }

    fn try_reserve(&mut self, _additional: usize) -> Result<(), TryReserveError> {
        self.buffer.try_reserve(_additional)
    }
}

#[cfg(feature = "raw-buffer")]
unsafe impl<S: Slice + ?Sized, B: RawBuffer<S>> RawBuffer<S> for BufferWithMetadata<B, ()> {
    fn into_raw(self) -> *const () {
        self.buffer.into_raw()
    }

    unsafe fn from_raw(ptr: *const ()) -> Self {
        Self::new(unsafe { B::from_raw(ptr) }, ())
    }
}

unsafe impl<B: Any, M: Any> DynBuffer for BufferWithMetadata<B, M> {
    type Buffer = B;
    type Metadata = M;

    fn get_metadata(&self) -> &Self::Metadata {
        &self.metadata
    }

    unsafe fn take_buffer(this: *mut Self, buffer: NonNull<()>) {
        unsafe { ptr::copy_nonoverlapping(addr_of!((*this).buffer), buffer.as_ptr().cast(), 1) }
        unsafe { ptr::drop_in_place(addr_of_mut!((*this).metadata)) }
    }
}

/// A wrapper around buffer implementing [`AsRef`].
#[derive(Debug, Clone)]
pub struct AsRefBuffer<B>(pub B);

impl<S: ?Sized, B: AsRef<S> + Send + 'static> Buffer<S> for AsRefBuffer<B> {
    fn as_slice(&self) -> &S {
        self.0.as_ref()
    }

    fn is_unique(&self) -> bool {
        false
    }
}

impl<B: BorrowMetadata> BorrowMetadata for AsRefBuffer<B> {
    type Metadata = B::Metadata;

    fn borrow_metadata(&self) -> &Self::Metadata {
        self.0.borrow_metadata()
    }
}

#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
const _: () = {
    #[cfg(not(feature = "portable-atomic"))]
    use alloc::sync::Arc;

    #[cfg(feature = "portable-atomic-util")]
    use portable_atomic_util::Arc;

    impl<B: BorrowMetadata + Send> BorrowMetadata for Arc<B> {
        type Metadata = B::Metadata;
        fn borrow_metadata(&self) -> &Self::Metadata {
            self.as_ref().borrow_metadata()
        }
    }

    impl<S: ?Sized, B: Buffer<S> + Sync> Buffer<S> for Arc<B> {
        fn as_slice(&self) -> &S {
            self.as_ref().as_slice()
        }

        fn is_unique(&self) -> bool {
            // See impl Buffer<T> for Arc<[T]>
            false
        }
    }

    #[cfg(feature = "raw-buffer")]
    unsafe impl<T: Send + Sync + 'static, B: Buffer<T> + Sync> RawBuffer<T> for Arc<B> {
        fn into_raw(self) -> *const () {
            Arc::into_raw(self).cast()
        }

        unsafe fn from_raw(ptr: *const ()) -> Self {
            unsafe { Arc::from_raw(ptr.cast()) }
        }
    }
};

mod private {
    use core::{any::Any, ptr::NonNull};

    #[allow(clippy::missing_safety_doc)]
    pub unsafe trait DynBuffer {
        type Buffer: Any;
        type Metadata: Any;
        fn get_metadata(&self) -> &Self::Metadata;
        unsafe fn take_buffer(this: *mut Self, buffer: NonNull<()>);
    }
}
