use std::{
    ptr,
    sync::{
        atomic::{AtomicPtr, Ordering},
        Arc,
    },
    thread,
};

use arc_slice::{
    layout::{DefaultLayout, VecLayout},
    ArcBytes, ArcSlice,
};

#[test]
fn arc_slice_vec_concurrent_clone() {
    let bytes = Arc::new(ArcBytes::<VecLayout>::from(vec![42]));
    let bytes2 = Arc::clone(&bytes);
    let thread = thread::spawn(move || {
        assert!(bytes2.metadata::<()>().is_none());
        (*bytes2).clone()
    });
    assert!(bytes.metadata::<()>().is_none());
    let clone1 = (*bytes).clone();
    let clone2 = thread.join().unwrap();
    drop(clone1);
    drop(clone2);
    let bytes = Arc::try_unwrap(bytes).unwrap();
    assert_eq!(bytes.try_into_buffer::<Vec<u8>>().unwrap(), [42]);
}

struct AtomicBox<T>(AtomicPtr<T>);
impl<T> AtomicBox<T> {
    fn new(value: Box<T>) -> Self {
        AtomicBox(AtomicPtr::new(Box::into_raw(value)))
    }
    fn take(&self) -> Option<Box<T>> {
        let ptr = self.0.swap(ptr::null_mut(), Ordering::Relaxed);
        (!ptr.is_null()).then(|| unsafe { Box::from_raw(ptr) })
    }
}
impl<T> Drop for AtomicBox<T> {
    fn drop(&mut self) {
        let ptr = *self.0.get_mut();
        if !ptr.is_null() {
            drop(unsafe { Box::from_raw(ptr) });
        }
    }
}

// miri doesn't catch error when `AtomicBox::take` is called in the thread

#[test]
fn arc_slice_drop() {
    let bytes = ArcSlice::<_, DefaultLayout>::from([AtomicBox::new(Box::new(42))]);
    let bytes2 = bytes.clone();
    let thread = thread::spawn(move || {
        drop(bytes2);
    });
    bytes[0].take();
    drop(bytes);
    thread.join().unwrap();
}

#[test]
fn arc_slice_drop_with_unique_hint() {
    let bytes = ArcSlice::<_, DefaultLayout>::from([AtomicBox::new(Box::new(42))]);
    let bytes2 = bytes.clone();
    let thread = thread::spawn(move || {
        bytes2.drop_with_unique_hint();
    });
    bytes[0].take();
    drop(bytes);
    thread.join().unwrap();
}
