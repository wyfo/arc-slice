#[cfg(not(all(loom, test)))]
#[cfg(not(feature = "portable-atomic"))]
pub(crate) use core::sync;

#[cfg(not(all(loom, test)))]
#[cfg(feature = "portable-atomic")]
pub(crate) mod sync {
    pub(crate) mod atomic {
        pub(crate) use portable_atomic::*;
    }
}

#[cfg(not(all(loom, test)))]
pub(crate) fn atomic_ptr_with_mut<T, R>(
    atomic: &mut sync::atomic::AtomicPtr<T>,
    f: impl FnOnce(&mut *mut T) -> R,
) -> R {
    f(atomic.get_mut())
}

#[cfg(not(all(loom, test)))]
pub(crate) fn atomic_usize_with_mut<R>(
    atomic: &mut sync::atomic::AtomicUsize,
    f: impl FnOnce(&mut usize) -> R,
) -> R {
    f(atomic.get_mut())
}

#[cfg(all(loom, test))]
pub(crate) use loom::sync;

#[cfg(all(loom, test))]
pub(crate) fn atomic_ptr_with_mut<T, R>(
    atomic: &mut sync::atomic::AtomicPtr<T>,
    f: impl FnOnce(&mut *mut T) -> R,
) -> R {
    atomic.with_mut(f)
}

#[cfg(all(loom, test))]
pub(crate) fn atomic_usize_with_mut<R>(
    atomic: &mut sync::atomic::AtomicUsize,
    f: impl FnOnce(&mut usize) -> R,
) -> R {
    atomic.with_mut(f)
}

#[cfg(all(loom, test))]
mod tests {
    use loom::{sync::Arc, thread};

    use crate::ArcBytes;

    #[test]
    fn arc_slice_vec_concurrent_clone() {
        loom::model(|| {
            let bytes = Arc::new(<ArcBytes>::new(alloc::vec![42]));
            let bytes2 = Arc::clone(&bytes);
            let thread = thread::spawn(move || {
                assert!(bytes2.get_metadata::<()>().is_some());
                (*bytes2).clone()
            });
            assert!(bytes.get_metadata::<()>().is_some());
            let _clone1 = (*bytes).clone();
            let _clone2 = thread.join().unwrap();
        });
    }
}
