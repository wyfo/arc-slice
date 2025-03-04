use alloc::{borrow::Cow, boxed::Box, string::String, vec::Vec};
use core::{mem, ptr, ptr::NonNull};

use crate::error::TryReserveError;

pub trait Buffer<T>: Send + 'static {
    fn as_slice(&self) -> &[T];

    #[doc(hidden)]
    #[inline(always)]
    fn is_array(&self) -> bool {
        false
    }

    #[doc(hidden)]
    #[inline(always)]
    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        Err(self)
    }

    #[doc(hidden)]
    #[inline(always)]
    fn try_into_vec(self) -> Result<Vec<T>, Self>
    where
        Self: Sized,
    {
        Err(self)
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for &'static [T] {
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline(always)]
    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl<T: Send + Sync + 'static, const N: usize> Buffer<T> for [T; N] {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline]
    fn is_array(&self) -> bool {
        true
    }
}

impl<T: Send + Sync + 'static, const N: usize> Buffer<T> for &'static [T; N] {
    #[inline]
    fn as_slice(&self) -> &[T] {
        *self
    }

    #[inline(always)]
    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for Box<[T]> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline(always)]
    fn try_into_vec(self) -> Result<Vec<T>, Self>
    where
        Self: Sized,
    {
        Ok(self.into_vec())
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for Vec<T> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline(always)]
    fn try_into_vec(self) -> Result<Vec<T>, Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl<T: Clone + Send + Sync + 'static> Buffer<T> for Cow<'static, [T]> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }

    #[inline(always)]
    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        match self {
            Cow::Borrowed(s) => Ok(s),
            cow => Err(cow),
        }
    }

    #[inline(always)]
    fn try_into_vec(self) -> Result<Vec<T>, Self>
    where
        Self: Sized,
    {
        match self {
            Cow::Owned(vec) => Ok(vec),
            cow => Err(cow),
        }
    }
}

#[cfg(not(feature = "portable-atomic"))]
impl<T: Send + Sync + 'static> Buffer<T> for alloc::sync::Arc<[T]> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        self
    }
}

/// # Safety
///
/// - [`as_mut_ptr`] must point to the start of a memory buffer of [`capacity`],
///   with the first [`len`] element initialized.
/// - slice delimited by [`as_mut_ptr`] and [`len`] must be the same as [`Buffer::as_slice`]
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

pub trait StringBuffer: Send + 'static {
    fn as_str(&self) -> &str;

    #[doc(hidden)]
    #[inline(always)]
    fn try_into_static(self) -> Result<&'static str, Self>
    where
        Self: Sized,
    {
        Err(self)
    }

    #[doc(hidden)]
    #[inline(always)]
    fn try_into_string(self) -> Result<String, Self>
    where
        Self: Sized,
    {
        Err(self)
    }
}

impl StringBuffer for &'static str {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline(always)]
    fn try_into_static(self) -> Result<&'static str, Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl StringBuffer for Box<str> {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline(always)]
    fn try_into_string(self) -> Result<String, Self>
    where
        Self: Sized,
    {
        Ok(self.into_string())
    }
}

impl StringBuffer for String {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline(always)]
    fn try_into_string(self) -> Result<String, Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl StringBuffer for Cow<'static, str> {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }

    #[inline(always)]
    fn try_into_static(self) -> Result<&'static str, Self>
    where
        Self: Sized,
    {
        match self {
            Cow::Borrowed(s) => Ok(s),
            cow => Err(cow),
        }
    }

    #[inline(always)]
    fn try_into_string(self) -> Result<String, Self>
    where
        Self: Sized,
    {
        match self {
            Cow::Owned(s) => Ok(s),
            cow => Err(cow),
        }
    }
}
