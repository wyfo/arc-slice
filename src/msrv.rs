use alloc::boxed::Box;
use core::num::NonZeroUsize;

use ptr::NonNull;

#[allow(dead_code)]
pub(crate) trait StrictProvenance<T>: Sized + Copy {
    type Addr;
    fn addr(self) -> Self::Addr;
    fn with_addr(self, addr: Self::Addr) -> Self;
    fn map_addr(self, f: impl FnOnce(Self::Addr) -> Self::Addr) -> Self {
        self.with_addr(f(self.addr()))
    }
}

pub(crate) trait SubPtrExt<T>: Sized + Copy {
    type Origin;
    unsafe fn sub_ptr(self, origin: Self::Origin) -> usize;
}

impl<T> SubPtrExt<T> for *const T {
    type Origin = *const T;
    unsafe fn sub_ptr(self, origin: Self::Origin) -> usize {
        unsafe { self.offset_from(origin).try_into().unwrap_unchecked() }
    }
}

impl<T> SubPtrExt<T> for *mut T {
    type Origin = *const T;
    unsafe fn sub_ptr(self, origin: *const T) -> usize {
        unsafe { self.offset_from(origin).try_into().unwrap_unchecked() }
    }
}

#[allow(unstable_name_collisions)]
impl<T> SubPtrExt<T> for NonNull<T> {
    type Origin = NonNull<T>;
    unsafe fn sub_ptr(self, origin: Self::Origin) -> usize {
        unsafe { self.as_ptr().sub_ptr(origin.as_ptr()) }
    }
}

#[allow(dead_code)]
pub(crate) trait NonNullExt<T>: Sized + Copy {
    unsafe fn add(self, count: usize) -> NonNull<T>;
    unsafe fn write(self, val: T);
}

impl<T> NonNullExt<T> for NonNull<T> {
    unsafe fn add(self, count: usize) -> NonNull<T> {
        unsafe { NonNull::new_unchecked(self.as_ptr().add(count)) }
    }

    unsafe fn write(self, val: T) {
        unsafe { self.as_ptr().write(val) }
    }
}

pub(crate) trait BoxExt<T: ?Sized> {
    fn into_non_null(self) -> NonNull<T>;
}

impl<T: ?Sized> BoxExt<T> for Box<T> {
    fn into_non_null(self) -> NonNull<T> {
        NonNull::new(Box::into_raw(self)).unwrap()
    }
}

pub(crate) trait Zeroable {
    type NonZero;
    fn non_zero(self) -> Option<Self::NonZero>;
    fn get(n: Self::NonZero) -> Self;
}

impl Zeroable for usize {
    type NonZero = NonZeroUsize;
    fn non_zero(self) -> Option<Self::NonZero> {
        NonZeroUsize::new(self)
    }
    fn get(n: Self::NonZero) -> Self {
        n.get()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct NonZero<T: Zeroable>(T::NonZero);

impl<T: Zeroable> NonZero<T> {
    pub(crate) fn new(n: T) -> Option<Self> {
        T::non_zero(n).map(Self)
    }

    pub(crate) unsafe fn new_unchecked(n: T) -> Self {
        unsafe { Self::new(n).unwrap_unchecked() }
    }

    pub(crate) fn get(self) -> T {
        T::get(self.0)
    }
}

impl From<NonZero<usize>> for NonZeroUsize {
    fn from(value: NonZero<usize>) -> Self {
        value.0
    }
}

pub(crate) mod ptr {
    pub(crate) use core::ptr::*;

    use crate::msrv::{NonZero, StrictProvenance};

    impl<T> StrictProvenance<T> for *mut T {
        type Addr = usize;
        fn addr(self) -> Self::Addr {
            self as usize
        }
        fn with_addr(self, addr: Self::Addr) -> Self {
            let ptr_addr = self as isize;
            let dest_addr = addr as isize;
            let offset = dest_addr.wrapping_sub(ptr_addr);
            self.cast::<u8>().wrapping_offset(offset).cast()
        }
    }

    #[allow(clippy::incompatible_msrv)]
    impl<T> StrictProvenance<T> for NonNull<T> {
        type Addr = NonZero<usize>;
        fn addr(self) -> Self::Addr {
            NonZero::new(self.as_ptr().addr()).unwrap()
        }
        fn with_addr(self, addr: Self::Addr) -> Self {
            unsafe { NonNull::new_unchecked(self.as_ptr().with_addr(addr.get())) }
        }
    }

    #[cfg(feature = "inlined")]
    pub(crate) const fn from_ref<T: ?Sized>(t: &T) -> *const T {
        t as _
    }

    pub(crate) fn from_mut<T: ?Sized>(t: &mut T) -> *mut T {
        t as _
    }

    pub(crate) const fn without_provenance_mut<T>(addr: usize) -> *mut T {
        null_mut::<u8>().wrapping_add(addr).cast()
    }
}
