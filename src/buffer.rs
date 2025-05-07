#[cfg(not(feature = "portable-atomic"))]
use alloc::sync::Arc;
use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    any::TypeId,
    mem, ptr,
    ptr::{addr_of, addr_of_mut, NonNull},
};

#[cfg(feature = "portable-atomic-util")]
use portable_atomic_util::Arc;

pub(crate) use crate::buffer::private::DynBuffer;
#[allow(unused_imports)]
use crate::msrv::SlicePtrExt;
use crate::{
    error::TryReserveError,
    layout::AnyBufferLayout,
    macros::{is, is_not},
    str::ArcStr,
    ArcSlice,
};

pub trait BorrowMetadata {
    type Metadata: Sync + 'static;

    fn borrow_metadata(&self) -> &Self::Metadata;
}

#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
impl<B: BorrowMetadata> BorrowMetadata for Arc<B> {
    type Metadata = B::Metadata;
    fn borrow_metadata(&self) -> &Self::Metadata {
        self.as_ref().borrow_metadata()
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct ArrayPtr<T>(pub(crate) *mut [T]);

pub trait Buffer<T>: Sized + Send + Sync + 'static {
    fn as_slice(&self) -> &[T];

    fn is_unique(&self) -> bool;

    #[doc(hidden)]
    #[inline(always)]
    fn is_array(&self) -> bool {
        false
    }

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_slice<L: AnyBufferLayout>(self) -> ArcSlice<T, L>
    where
        T: Send + Sync + 'static,
    {
        ArcSlice::from_buffer_impl(BufferWithMetadata::new(self, ()))
    }

    #[doc(hidden)]
    #[inline(always)]
    unsafe fn try_from_array(_array: ArrayPtr<T>) -> Option<Self> {
        None
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for &'static [T] {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        false
    }

    #[inline(always)]
    fn into_arc_slice<L: AnyBufferLayout>(self) -> ArcSlice<T, L> {
        ArcSlice::from_static(self)
    }
}

impl<T: Send + Sync + 'static, const N: usize> Buffer<T> for [T; N] {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        true
    }

    #[doc(hidden)]
    #[inline(always)]
    fn is_array(&self) -> bool {
        true
    }

    #[doc(hidden)]
    fn into_arc_slice<L: AnyBufferLayout>(self) -> ArcSlice<T, L>
    where
        T: Send + Sync + 'static,
    {
        ArcSlice::new_array(self)
    }

    #[doc(hidden)]
    #[inline(always)]
    unsafe fn try_from_array(array: ArrayPtr<T>) -> Option<Self> {
        (array.0.len() == N).then(|| unsafe { ptr::read(array.0.cast()) })
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for Box<[T]> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        true
    }

    #[inline(always)]
    fn into_arc_slice<L: AnyBufferLayout>(self) -> ArcSlice<T, L> {
        ArcSlice::from_vec(self.into_vec())
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for Vec<T> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        true
    }

    #[inline(always)]
    fn into_arc_slice<L: AnyBufferLayout>(self) -> ArcSlice<T, L> {
        ArcSlice::from_vec(self)
    }
}

#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
impl<T: Send + Sync + 'static> Buffer<T> for Arc<[T]> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        // Arc doesn't expose an API to check uniqueness with shared reference
        // See `Arc::is_unique`, it cannot be done by simply checking strong/weak counts
        false
    }
}

#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
impl<T: Send + Sync + 'static, B: Buffer<T>> Buffer<T> for Arc<B> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self.as_ref().as_slice()
    }

    #[inline]
    fn is_unique(&self) -> bool {
        // See impl Buffer<T> for Arc<[T]>
        false
    }
}

/// # Safety
///
/// - [`as_mut_ptr`] must point to the start of a memory buffer of [`capacity`],
///   with the first [`len`] element initialized.
/// - slice delimited by [`as_mut_ptr`] and [`len`] must be the same as [`Buffer::as_slice`]
/// - [`Buffer::is_unique`] must return `true`
///
/// [`as_mut_ptr`]: Self::as_mut_ptr
/// [`capacity`]: Self::capacity
/// [`len`]: Self::len
#[allow(clippy::len_without_is_empty)]
pub unsafe trait BufferMut<T>: Buffer<T> {
    fn as_mut_ptr(&mut self) -> NonNull<T>;

    fn len(&self) -> usize;

    fn capacity(&self) -> usize;

    /// # Safety
    ///
    /// - First `len` items of buffer slice must be initialized.
    /// - If `mem::needs_drop::<T>()`, then `len` must be greater or equal to [`Self::len`].
    unsafe fn set_len(&mut self, len: usize) -> bool;

    fn reserve(&mut self, _additional: usize) -> bool;
}

unsafe impl<T: Send + Sync + 'static> BufferMut<T> for Vec<T> {
    #[inline]
    fn as_mut_ptr(&mut self) -> NonNull<T> {
        NonNull::new(self.as_mut_ptr()).unwrap()
    }

    #[inline]
    fn len(&self) -> usize {
        self.len()
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.capacity()
    }

    #[inline]
    unsafe fn set_len(&mut self, len: usize) -> bool {
        // SAFETY: same function contract
        unsafe { self.set_len(len) };
        true
    }

    #[inline]
    fn reserve(&mut self, additional: usize) -> bool {
        self.reserve(additional);
        true
    }
}

unsafe impl<T: Send + Sync + 'static, const N: usize> BufferMut<T> for [T; N] {
    #[inline]
    fn as_mut_ptr(&mut self) -> NonNull<T> {
        NonNull::new(self.as_mut_slice().as_mut_ptr()).unwrap()
    }

    #[inline]
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    unsafe fn set_len(&mut self, _len: usize) -> bool {
        false
    }

    #[inline]
    fn reserve(&mut self, _additional: usize) -> bool {
        false
    }
}

pub(crate) trait BufferMutExt<T>: BufferMut<T> + Sized {
    unsafe fn shift_left(&mut self, offset: usize, length: usize) -> bool {
        let prev_len = self.len();
        if !unsafe { self.set_len(length) } {
            return false;
        }
        let buffer_ptr = self.as_mut_ptr().as_ptr();
        if mem::needs_drop::<T>() {
            unsafe {
                ptr::drop_in_place(ptr::slice_from_raw_parts_mut(buffer_ptr, offset));
                if prev_len > offset + length {
                    ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                        buffer_ptr.add(offset + length),
                        prev_len - offset - length,
                    ));
                }
            }
        }
        if offset >= length {
            unsafe { ptr::copy_nonoverlapping(buffer_ptr.add(offset), buffer_ptr, length) };
        } else {
            unsafe { ptr::copy(buffer_ptr.add(offset), buffer_ptr, length) };
        }
        true
    }

    unsafe fn try_reclaim(&mut self, offset: usize, length: usize, additional: usize) -> bool {
        self.capacity() - length >= additional
            && offset >= length
            && unsafe { self.shift_left(offset, length) }
    }

    unsafe fn try_reclaim_or_reserve(
        &mut self,
        offset: usize,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> Result<usize, TryReserveError> {
        let capacity = self.capacity();
        if capacity - offset - length >= additional {
            return Ok(offset);
        }
        // conditions from `BytesMut::reserve_inner`
        if self.capacity() - length >= additional
            && offset >= length
            && unsafe { self.shift_left(offset, length) }
        {
            return Ok(0);
        }
        if allocate && unsafe { self.shift_left(0, offset + length) } && self.reserve(additional) {
            Ok(offset)
        } else {
            Err(TryReserveError::Unsupported)
        }
    }
}

impl<T, B: BufferMut<T>> BufferMutExt<T> for B {}

pub trait StringBuffer: Sized + Send + Sync + 'static {
    fn as_str(&self) -> &str;

    fn is_unique(&self) -> bool;

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_str<L: AnyBufferLayout>(self) -> ArcStr<L> {
        unsafe { ArcStr::from_utf8_unchecked(StringBufferWrapper(self).into_arc_slice()) }
    }
}

impl StringBuffer for &'static str {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        false
    }

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_str<L: AnyBufferLayout>(self) -> ArcStr<L> {
        unsafe { ArcStr::from_utf8_unchecked(self.as_bytes().into_arc_slice()) }
    }
}

impl StringBuffer for Box<str> {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        true
    }

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_str<L: AnyBufferLayout>(self) -> ArcStr<L> {
        unsafe { ArcStr::from_utf8_unchecked(self.into_boxed_bytes().into_arc_slice()) }
    }
}

impl StringBuffer for String {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        true
    }

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_str<L: AnyBufferLayout>(self) -> ArcStr<L> {
        unsafe { ArcStr::from_utf8_unchecked(self.into_bytes().into_arc_slice()) }
    }
}

#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
impl StringBuffer for Arc<str> {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline]
    fn is_unique(&self) -> bool {
        // See impl Buffer<T> for Arc<[T]>
        false
    }
}

#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
impl<B: StringBuffer + Send + Sync> StringBuffer for Arc<B> {
    #[inline]
    fn as_str(&self) -> &str {
        self.as_ref().as_str()
    }

    #[inline]
    fn is_unique(&self) -> bool {
        // See impl Buffer<T> for Arc<[T]>
        false
    }
}

#[derive(Clone)]
pub(crate) struct StringBufferWrapper<B>(pub(crate) B);

impl<B: StringBuffer> Buffer<u8> for StringBufferWrapper<B> {
    fn as_slice(&self) -> &[u8] {
        self.0.as_str().as_bytes()
    }

    fn is_unique(&self) -> bool {
        self.0.is_unique()
    }
}

#[cfg(feature = "raw-buffer")]
unsafe impl<B: RawStringBuffer> RawBuffer<u8> for StringBufferWrapper<B> {
    fn into_raw(self) -> *const () {
        self.0.into_raw()
    }

    unsafe fn from_raw(ptr: *const ()) -> Self {
        Self(unsafe { B::from_raw(ptr) })
    }
}

impl<B: BorrowMetadata> BorrowMetadata for StringBufferWrapper<B> {
    type Metadata = B::Metadata;

    fn borrow_metadata(&self) -> &Self::Metadata {
        self.0.borrow_metadata()
    }
}

#[cfg(feature = "raw-buffer")]
/// # Safety
///
/// - slice returned by [`Buffer::as_slice`] must not be invalidated by [`RawBuffer::into_raw`]
/// - if [`BorrowMetadata`] is implemented, metadata returned by
///   [`BorrowMetadata::borrow_metadata`] must not be invalidated by [`RawBuffer::into_raw`]
pub unsafe trait RawBuffer<T>: Buffer<T> + Clone {
    fn into_raw(self) -> *const ();
    /// # Safety
    /// The pointer must be obtained by a call to [`RawBuffer::into_raw`].
    unsafe fn from_raw(ptr: *const ()) -> Self;
}

#[cfg(feature = "raw-buffer")]
#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
unsafe impl<T: Send + Sync + 'static, B: Buffer<T>> RawBuffer<T> for Arc<B> {
    #[inline]
    fn into_raw(self) -> *const () {
        Arc::into_raw(self).cast()
    }

    #[inline]
    unsafe fn from_raw(ptr: *const ()) -> Self {
        unsafe { Arc::from_raw(ptr.cast()) }
    }
}

/// # Safety
///
/// - slice returned by [`StringBuffer::as_str`] must not be invalidated by [`RawStringBuffer::into_raw`]
/// - if [`BorrowMetadata`] is implemented, metadata returned by
///   [`BorrowMetadata::borrow_metadata`] must not be invalidated by [`RawStringBuffer::into_raw`]
pub unsafe trait RawStringBuffer: StringBuffer + Clone {
    fn into_raw(self) -> *const ();
    /// # Safety
    /// The pointer must be obtained by a call to [`RawBuffer::into_raw`].
    unsafe fn from_raw(ptr: *const ()) -> Self;
}

#[cfg(any(not(feature = "portable-atomic"), feature = "portable-atomic-util"))]
unsafe impl<B: StringBuffer + Send + Sync> RawStringBuffer for Arc<B> {
    #[inline]
    fn into_raw(self) -> *const () {
        Arc::into_raw(self).cast()
    }
    #[inline]
    unsafe fn from_raw(ptr: *const ()) -> Self {
        unsafe { Arc::from_raw(ptr.cast()) }
    }
}

unsafe impl<B: BorrowMetadata + 'static> DynBuffer for B {
    fn has_metadata() -> bool {
        is_not!(B::Metadata, ())
    }

    fn get_metadata(&self, type_id: TypeId) -> Option<NonNull<()>> {
        is!({ type_id }, B).then(|| NonNull::from(self.borrow_metadata()).cast())
    }

    unsafe fn take_buffer(this: *mut Self, type_id: TypeId, buffer: NonNull<()>) -> bool {
        if is!({ type_id }, B) {
            unsafe { ptr::copy_nonoverlapping(this, buffer.as_ptr().cast(), 1) }
            return true;
        }
        false
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
}

impl<B> From<B> for BufferWithMetadata<B, ()> {
    fn from(value: B) -> Self {
        Self::new(value, ())
    }
}

impl<T, B: Buffer<T>, M: Send + Sync + 'static> Buffer<T> for BufferWithMetadata<B, M> {
    fn as_slice(&self) -> &[T] {
        self.buffer.as_slice()
    }

    fn is_unique(&self) -> bool {
        self.buffer.is_unique()
    }
}

unsafe impl<T, B: BufferMut<T>, M: Send + Sync + 'static> BufferMut<T>
    for BufferWithMetadata<B, M>
{
    fn as_mut_ptr(&mut self) -> NonNull<T> {
        self.buffer.as_mut_ptr()
    }

    fn len(&self) -> usize {
        self.buffer.len()
    }

    fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    unsafe fn set_len(&mut self, len: usize) -> bool {
        unsafe { self.buffer.set_len(len) }
    }

    fn reserve(&mut self, _additional: usize) -> bool {
        self.buffer.reserve(_additional)
    }
}

#[cfg(feature = "raw-buffer")]
unsafe impl<T, B: RawBuffer<T>> RawBuffer<T> for BufferWithMetadata<B, ()> {
    fn into_raw(self) -> *const () {
        self.buffer.into_raw()
    }

    unsafe fn from_raw(ptr: *const ()) -> Self {
        Self {
            buffer: unsafe { B::from_raw(ptr) },
            metadata: (),
        }
    }
}

unsafe impl<B: 'static, M: 'static> DynBuffer for BufferWithMetadata<B, M> {
    fn has_metadata() -> bool {
        is_not!(M, ())
    }

    fn get_metadata(&self, type_id: TypeId) -> Option<NonNull<()>> {
        is!({ type_id }, M).then(|| NonNull::from(&self.metadata).cast())
    }

    unsafe fn take_buffer(this: *mut Self, type_id: TypeId, buffer: NonNull<()>) -> bool {
        if is!({ type_id }, B) {
            unsafe { ptr::copy_nonoverlapping(addr_of!((*this).buffer), buffer.as_ptr().cast(), 1) }
            unsafe { ptr::drop_in_place(addr_of_mut!((*this).metadata)) }
            return true;
        }
        false
    }
}

pub(crate) mod private {
    use core::{any::TypeId, ptr::NonNull};

    /// # Safety
    ///
    /// TODO
    pub unsafe trait DynBuffer {
        fn has_metadata() -> bool;
        fn get_metadata(&self, type_id: TypeId) -> Option<NonNull<()>>;
        unsafe fn take_buffer(this: *mut Self, type_id: TypeId, buffer: NonNull<()>) -> bool;
    }
}
