//! The errors.

use core::fmt;

pub(crate) use private::AllocErrorImpl;

/// The memory allocator returned an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocError;

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("allocation error")
    }
}

/// Error which can occur when trying to reserve additional capacity for a mutable buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TryReserveError {
    /// The buffer reference is not unique.
    ///
    /// It may occur when the same buffer is reference by multiple `ArcSliceMut`.
    NotUnique,
    /// The buffer doesn't support reservation.
    Unsupported,
    /// The memory allocator returned an error.
    AllocError,
    /// The required capacity exceeds the buffer maximum (usually isize::MAX bytes).
    CapacityOverflow,
}

impl From<AllocError> for TryReserveError {
    fn from(_: AllocError) -> Self {
        Self::AllocError
    }
}

impl fmt::Display for TryReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUnique => f.write_str("not unique"),
            Self::Unsupported => f.write_str("unsupported"),
            Self::AllocError => f.write_str("allocation error"),
            Self::CapacityOverflow => f.write_str("capacity overflow"),
        }
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;
    impl std::error::Error for AllocError {}
    impl std::error::Error for TryReserveError {}
};

mod private {
    use alloc::alloc::{alloc, alloc_zeroed, handle_alloc_error};
    use core::{alloc::Layout, convert::Infallible, mem, ptr::NonNull};

    use crate::{error::AllocError, utils::assert_checked};

    pub trait AllocErrorImpl: Sized {
        const FALLIBLE: bool;
        fn forget<T>(self, x: T) -> Self {
            mem::forget(x);
            self
        }
        fn capacity_overflow() -> Self;
        fn alloc<T, const ZEROED: bool>(layout: Layout) -> Result<NonNull<T>, Self>;
    }

    impl AllocErrorImpl for AllocError {
        const FALLIBLE: bool = true;
        fn capacity_overflow() -> Self {
            Self
        }
        fn alloc<T, const ZEROED: bool>(layout: Layout) -> Result<NonNull<T>, Self> {
            assert_checked(layout.size() > 0);
            let ptr = unsafe { (if ZEROED { alloc_zeroed } else { alloc })(layout) };
            Ok(NonNull::new(ptr).ok_or(AllocError)?.cast())
        }
    }

    impl AllocErrorImpl for Infallible {
        const FALLIBLE: bool = false;
        #[cold]
        #[inline(never)]
        fn capacity_overflow() -> Self {
            panic!("capacity overflow")
        }
        fn alloc<T, const ZEROED: bool>(layout: Layout) -> Result<NonNull<T>, Self> {
            AllocError::alloc::<T, ZEROED>(layout).map_err(|_| handle_alloc_error(layout))
        }
    }
}
