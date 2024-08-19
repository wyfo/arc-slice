use alloc::{borrow::Cow, boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{fmt, ptr::NonNull};

pub trait Buffer<T>: Send + 'static {
    fn as_slice(&self) -> &[T];

    #[doc(hidden)]
    fn is_array(&self) -> bool {
        false
    }

    #[doc(hidden)]
    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        Err(self)
    }

    #[doc(hidden)]
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

    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl<T: Send + Sync + 'static, const N: usize> Buffer<T> for [T; N] {
    fn as_slice(&self) -> &[T] {
        self
    }

    fn is_array(&self) -> bool {
        true
    }
}

impl<T: Send + Sync + 'static, const N: usize> Buffer<T> for &'static [T; N] {
    fn as_slice(&self) -> &[T] {
        *self
    }

    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for Box<[T]> {
    fn as_slice(&self) -> &[T] {
        self
    }

    fn try_into_vec(self) -> Result<Vec<T>, Self>
    where
        Self: Sized,
    {
        Ok(self.into_vec())
    }
}

impl<T: Send + Sync + 'static> Buffer<T> for Vec<T> {
    fn as_slice(&self) -> &[T] {
        self
    }

    fn try_into_vec(self) -> Result<Vec<T>, Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl<T: Clone + Send + Sync + 'static> Buffer<T> for Cow<'static, [T]> {
    fn as_slice(&self) -> &[T] {
        self
    }

    fn try_into_static(self) -> Result<&'static [T], Self>
    where
        Self: Sized,
    {
        match self {
            Cow::Borrowed(s) => Ok(s),
            cow => Err(cow),
        }
    }

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

impl<T: Send + Sync + 'static> Buffer<T> for Arc<[T]> {
    fn as_slice(&self) -> &[T] {
        self
    }
}

/// # Safety
///
/// - [`as_mut_ptr`] must point to the start of a memory buffer of [`capacity`],
///   with the first [`len`] element initialized.
/// - slice delimited by [`as_mut_ptr`] and [`len`] must be the same as [`Buffer::as_slice`]
/// - [`shift_left`], [`truncate`] and [`set_len`] must behave as expected
///
/// [`as_mut_ptr`]: Self::as_mut_ptr
/// [`capacity`]: Self::capacity
/// [`len`]: Self::len
/// [`shift_left`]: Self::shift_left
/// [`truncate`]: Self::truncate
/// [`set_len`]: Self::set_len
#[allow(clippy::len_without_is_empty)]
pub unsafe trait BufferMut<T>: Send + 'static {
    fn as_mut_ptr(&mut self) -> NonNull<T>;

    fn len(&self) -> usize;

    fn capacity(&self) -> usize;

    fn shift_left(&mut self, by: usize) -> bool;

    fn truncate(&mut self, len: usize) -> bool;

    /// # Safety
    ///
    /// - First `len` items of buffer slice must be initialized.
    /// - If `mem::needs_drop::<T>()`, then `len` must be greater or equal to [`Self::len`].
    unsafe fn set_len(&mut self, len: usize);

    fn try_reserve(&mut self, _additional: usize) -> Result<(), TryReserveError>;

    #[doc(hidden)]
    fn try_into_vec(self) -> Result<Vec<T>, Self>
    where
        Self: Sized,
    {
        Err(self)
    }
}

unsafe impl<T: Send + Sync + 'static> BufferMut<T> for Vec<T> {
    fn as_mut_ptr(&mut self) -> NonNull<T> {
        NonNull::new(self.as_mut_ptr()).unwrap()
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn capacity(&self) -> usize {
        self.capacity()
    }

    fn shift_left(&mut self, by: usize) -> bool {
        self.drain(..by);
        true
    }

    fn truncate(&mut self, len: usize) -> bool {
        self.truncate(len);
        true
    }

    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY: same function contract
        unsafe { self.set_len(len) }
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        let overflow = self.len().saturating_add(additional) > isize::MAX as usize;
        match self.try_reserve(additional) {
            Ok(_) => Ok(()),
            Err(_) if overflow => Err(TryReserveError::CapacityOverflow),
            Err(_) => Err(TryReserveError::AllocError),
        }
    }

    fn try_into_vec(self) -> Result<Vec<T>, Self> {
        Ok(self)
    }
}

unsafe impl<T: Send + Sync + 'static, const N: usize> BufferMut<T> for [T; N] {
    fn as_mut_ptr(&mut self) -> NonNull<T> {
        NonNull::new(self.as_mut_slice().as_mut_ptr()).unwrap()
    }

    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn capacity(&self) -> usize {
        self.as_slice().len()
    }

    fn shift_left(&mut self, _by: usize) -> bool {
        false
    }

    fn truncate(&mut self, _len: usize) -> bool {
        false
    }

    unsafe fn set_len(&mut self, _len: usize) {}

    fn try_reserve(&mut self, _additional: usize) -> Result<(), TryReserveError> {
        Err(TryReserveError::Unsupported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TryReserveError {
    AllocError,
    CapacityOverflow,
    NotUnique,
    Unsupported,
}

impl fmt::Display for TryReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocError => write!(f, "allocation error"),
            Self::CapacityOverflow => write!(f, "capacity overflow"),
            Self::NotUnique => write!(f, "not unique"),
            Self::Unsupported => write!(f, "unsupported"),
        }
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;
    impl std::error::Error for TryReserveError {}
};

// from `BytesMut::reserve_inner`
pub(crate) fn can_reclaim(
    full_capacity: usize,
    offset: usize,
    length: usize,
    additional: usize,
) -> bool {
    full_capacity - length >= additional && offset >= length
}

pub trait StringBuffer: Send + 'static {
    fn as_str(&self) -> &str;

    #[doc(hidden)]
    fn try_into_static(self) -> Result<&'static str, Self>
    where
        Self: Sized,
    {
        Err(self)
    }

    #[doc(hidden)]
    fn try_into_string(self) -> Result<String, Self>
    where
        Self: Sized,
    {
        Err(self)
    }
}

impl StringBuffer for &'static str {
    fn as_str(&self) -> &str {
        self
    }

    fn try_into_static(self) -> Result<&'static str, Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl StringBuffer for Box<str> {
    fn as_str(&self) -> &str {
        self
    }

    fn try_into_string(self) -> Result<String, Self>
    where
        Self: Sized,
    {
        Ok(self.into_string())
    }
}

impl StringBuffer for String {
    fn as_str(&self) -> &str {
        self
    }

    fn try_into_string(self) -> Result<String, Self>
    where
        Self: Sized,
    {
        Ok(self)
    }
}

impl StringBuffer for Cow<'static, str> {
    fn as_str(&self) -> &str {
        self
    }

    fn try_into_static(self) -> Result<&'static str, Self>
    where
        Self: Sized,
    {
        match self {
            Cow::Borrowed(s) => Ok(s),
            cow => Err(cow),
        }
    }

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
