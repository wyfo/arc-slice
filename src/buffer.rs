use alloc::{
    alloc::{handle_alloc_error, realloc},
    boxed::Box,
    string::String,
    vec::Vec,
};
use core::{
    alloc::{Layout, LayoutError},
    any::Any,
    cmp::max,
    marker::PhantomData,
    mem, ptr,
    ptr::{addr_of, addr_of_mut, NonNull},
};

pub(crate) use crate::buffer::private::DynBuffer;
#[allow(unused_imports)]
use crate::msrv::{NonNullExt, SlicePtrExt};
use crate::{
    error::TryReserveError,
    layout::{AnyBufferLayout, LayoutMut},
    slice_mut::TryReserveResult,
    str::ArcStr,
    utils::{assert_checked, NewChecked},
    ArcSlice, ArcSliceMut,
};

pub trait BorrowMetadata: Sync {
    type Metadata: Sync + 'static;

    fn borrow_metadata(&self) -> &Self::Metadata;
}

#[doc(hidden)]
#[derive(Debug)]
pub struct ArrayPtr<T>(pub(crate) *mut [T]);

pub trait Buffer<T>: Sized + Send + 'static {
    fn as_slice(&self) -> &[T];

    #[inline]
    fn is_unique(&self) -> bool {
        true
    }

    #[doc(hidden)]
    #[inline(always)]
    fn is_array() -> bool {
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

    #[doc(hidden)]
    #[inline(always)]
    fn is_array() -> bool {
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

    #[inline(always)]
    fn into_arc_slice<L: AnyBufferLayout>(self) -> ArcSlice<T, L> {
        ArcSlice::from_vec(self)
    }
}

/// # Safety
///
/// - [`as_mut_ptr`] must point to the start of a memory buffer of [`capacity`],
///   with the first [`len`] element initialized.
/// - slice delimited by [`as_mut_ptr`] and [`len`] must be the same as [`Buffer::as_slice`]
/// - retrieving [`capacity`] must not invalidate the buffer slice
/// - if the type implement [`BorrowMetadata`], then [`borrow_metadata`] must not invalidate the buffer slice
///
/// [`as_mut_ptr`]: Self::as_mut_ptr
/// [`capacity`]: Self::capacity
/// [`len`]: Self::len
/// [`borrow_metadata`]: BorrowMetadata::borrow_metadata
#[allow(clippy::len_without_is_empty)]
pub unsafe trait BufferMut<T>: Buffer<T> + Sync {
    fn as_mut_ptr(&mut self) -> NonNull<T>;

    fn len(&self) -> usize;

    fn capacity(&self) -> usize;

    /// # Safety
    ///
    /// - First `len` items of buffer slice must be initialized.
    /// - If `mem::needs_drop::<T>()`, then `len` must be greater or equal to [`Self::len`].
    unsafe fn set_len(&mut self, len: usize) -> bool;

    fn reserve(&mut self, additional: usize) -> bool;

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_slice_mut<L: AnyBufferLayout + LayoutMut>(self) -> ArcSliceMut<T, L>
    where
        T: Send + Sync + 'static,
    {
        ArcSliceMut::from_buffer_impl(BufferWithMetadata::new(self, ()))
    }
}

unsafe impl<T: Send + Sync + 'static> BufferMut<T> for Vec<T> {
    #[inline]
    fn as_mut_ptr(&mut self) -> NonNull<T> {
        NonNull::new_checked(self.as_mut_ptr())
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

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_slice_mut<L: AnyBufferLayout + LayoutMut>(self) -> ArcSliceMut<T, L> {
        ArcSliceMut::from_vec(self)
    }
}

unsafe impl<T: Send + Sync + 'static, const N: usize> BufferMut<T> for [T; N] {
    #[inline]
    fn as_mut_ptr(&mut self) -> NonNull<T> {
        NonNull::new_checked(self.as_mut_slice().as_mut_ptr())
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

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_slice_mut<L: AnyBufferLayout + LayoutMut>(self) -> ArcSliceMut<T, L> {
        ArcSliceMut::new_array(self)
    }
}

// pub(crate) trait BufferMutImpl<T> {
//     fn as_mut_ptr(&mut self) -> NonNull<T>;
//
//     fn len(&self) -> usize;
//
//     fn capacity(&self) -> usize;
//
//     unsafe fn set_len(&mut self, len: usize) -> bool;
//
//     fn reserve(&mut self, additional: usize) -> bool;
// }
//
// impl<T, B: BufferMut<T>> BufferMutImpl<T> for &mut B {
//     fn as_mut_ptr(&mut self) -> NonNull<T> {
//         B::as_mut_ptr(self)
//     }
//
//     fn len(&self) -> usize {
//         B::len(self)
//     }
//
//     fn capacity(&self) -> usize {
//         B::capacity(self)
//     }
//
//     unsafe fn set_len(&mut self, len: usize) -> bool {
//         unsafe { B::set_len(self, len) }
//     }
//
//     fn reserve(&mut self, additional: usize) -> bool {
//         B::reserve(self, additional)
//     }
// }

pub(crate) trait BufferMutExt<T>: BufferMut<T> + Sized {
    unsafe fn realloc(
        &mut self,
        additional: usize,
        ptr: NonNull<u8>,
        layout: impl Fn(usize) -> Result<Layout, LayoutError>,
    ) -> (NonNull<u8>, usize) {
        let required = self
            .len()
            .checked_add(additional)
            .unwrap_or_else(|| panic!("capacity overflow"));
        let new_capacity = max(self.capacity() * 2, required);
        let cur_layout = unsafe { layout(self.capacity()).unwrap_unchecked() };
        let new_layout = layout(new_capacity).unwrap_or_else(|_| panic!("capacity overflow"));
        let new_ptr = unsafe { realloc(ptr.as_ptr(), cur_layout, new_layout.size()) };
        if new_ptr.is_null() {
            handle_alloc_error(new_layout);
        }
        (NonNull::new_checked(new_ptr).cast(), new_capacity)
    }

    unsafe fn shift_left(&mut self, offset: usize, length: usize) -> bool {
        assert_checked(!mem::needs_drop::<T>());
        let prev_len = self.len();
        if length == prev_len {
            return true;
        }
        if !unsafe { self.set_len(length) } {
            return false;
        }
        let buffer_ptr = self.as_mut_ptr().as_ptr();
        if offset == 0 {
            return true;
        } else if offset >= length {
            unsafe { ptr::copy_nonoverlapping(buffer_ptr.add(offset), buffer_ptr, length) };
        } else {
            unsafe { ptr::copy(buffer_ptr.add(offset), buffer_ptr, length) };
        }
        true
    }

    unsafe fn try_reserve_impl(
        &mut self,
        offset: usize,
        length: usize,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<T> {
        let capacity = self.capacity();
        if capacity - offset - length >= additional {
            return (Ok(capacity - offset), unsafe {
                self.as_mut_ptr().add(offset)
            });
        }
        if !mem::needs_drop::<T>()
            // conditions from `BytesMut::reserve_inner`
            && self.capacity() - length >= additional
            && offset >= length
            && unsafe { self.shift_left(offset, length) }
        {
            return (Ok(capacity), self.as_mut_ptr());
        }
        if allocate && unsafe { self.set_len(offset + length) } && self.reserve(additional) {
            return (Ok(self.capacity() - offset), unsafe {
                self.as_mut_ptr().add(offset)
            });
        }
        (Err(TryReserveError::Unsupported), unsafe {
            self.as_mut_ptr().add(offset)
        })
    }
}

impl<T, B: BufferMut<T>> BufferMutExt<T> for B {}

pub trait StringBuffer: Sized + Send + 'static {
    fn as_str(&self) -> &str;

    #[inline]
    fn is_unique(&self) -> bool {
        true
    }

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

    #[doc(hidden)]
    #[inline(always)]
    fn into_arc_str<L: AnyBufferLayout>(self) -> ArcStr<L> {
        unsafe { ArcStr::from_utf8_unchecked(self.into_bytes().into_arc_slice()) }
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

#[derive(Debug, Clone)]
pub struct AsRefBuffer<B, const UNIQUE: bool = true>(pub B);

impl<T: Send + Sync + 'static, B: AsRef<[T]> + Send + 'static, const UNIQUE: bool> Buffer<T>
    for AsRefBuffer<B, UNIQUE>
{
    fn as_slice(&self) -> &[T] {
        self.0.as_ref()
    }

    fn is_unique(&self) -> bool {
        UNIQUE
    }
}

impl<B: AsRef<str> + Send + 'static, const UNIQUE: bool> StringBuffer for AsRefBuffer<B, UNIQUE> {
    fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    fn is_unique(&self) -> bool {
        UNIQUE
    }
}

pub trait BufferImpl<B>: Send + 'static {
    type Slice: ?Sized;
    type Metadata: Sync + 'static;
    fn buffer_slice(buffer: &B) -> &Self::Slice;
    fn buffer_is_unique(_buffer: &B) -> bool {
        false
    }
    fn borrow_metadata(buffer: &B) -> &Self::Metadata;
}

#[derive(Debug)]
pub struct BufferWithImpl<B, D> {
    pub buffer: B,
    _phantom: PhantomData<D>,
}

impl<B: Clone, D> Clone for BufferWithImpl<B, D> {
    fn clone(&self) -> Self {
        Self::new(self.buffer.clone())
    }
}

impl<B, D> BufferWithImpl<B, D> {
    pub fn new(buffer: B) -> Self {
        Self {
            buffer,
            _phantom: PhantomData,
        }
    }
}

impl<B: Sync, D: BufferImpl<B> + Sync> BorrowMetadata for BufferWithImpl<B, D> {
    type Metadata = D::Metadata;
    fn borrow_metadata(&self) -> &Self::Metadata {
        D::borrow_metadata(&self.buffer)
    }
}

impl<T: Send + Sync + 'static, B: Send + 'static, D: BufferImpl<B, Slice = [T]>> Buffer<T>
    for BufferWithImpl<B, D>
{
    fn as_slice(&self) -> &D::Slice {
        D::buffer_slice(&self.buffer)
    }

    fn is_unique(&self) -> bool {
        D::buffer_is_unique(&self.buffer)
    }
}

impl<B: Send + 'static, D: BufferImpl<B, Slice = str>> StringBuffer for BufferWithImpl<B, D> {
    fn as_str(&self) -> &D::Slice {
        D::buffer_slice(&self.buffer)
    }

    fn is_unique(&self) -> bool {
        D::buffer_is_unique(&self.buffer)
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

    impl<T: Send + Sync + 'static, B: Buffer<T> + Sync> Buffer<T> for Arc<B> {
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

    #[cfg(feature = "raw-buffer")]
    unsafe impl<T: Send + Sync + 'static, B: Buffer<T> + Sync> RawBuffer<T> for Arc<B> {
        #[inline]
        fn into_raw(self) -> *const () {
            Arc::into_raw(self).cast()
        }

        #[inline]
        unsafe fn from_raw(ptr: *const ()) -> Self {
            unsafe { Arc::from_raw(ptr.cast()) }
        }
    }

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

    #[cfg(feature = "raw-buffer")]
    unsafe impl<
            T: Send + Sync + 'static,
            B: Send + Sync + 'static,
            D: BufferImpl<Arc<B>, Slice = [T]>,
        > RawBuffer<T> for BufferWithImpl<Arc<B>, D>
    {
        fn into_raw(self) -> *const () {
            Arc::into_raw(self.buffer).cast()
        }

        unsafe fn from_raw(ptr: *const ()) -> Self {
            Self::new(unsafe { Arc::from_raw(ptr.cast()) })
        }
    }

    #[cfg(feature = "raw-buffer")]
    unsafe impl<B: Send + Sync + 'static, D: BufferImpl<Arc<B>, Slice = str>> RawStringBuffer
        for BufferWithImpl<Arc<B>, D>
    {
        fn into_raw(self) -> *const () {
            Arc::into_raw(self.buffer).cast()
        }

        unsafe fn from_raw(ptr: *const ()) -> Self {
            Self::new(unsafe { Arc::from_raw(ptr.cast()) })
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
