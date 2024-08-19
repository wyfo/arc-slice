use std::{
    mem, ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use arc_slice::{layout::Compact, ArcBytes};

// empty vec subslices doesn't trigger promotion to an arc, so it can still be downcasted
#[test]
fn empty_vec_subslices() {
    let mut bytes = <ArcBytes>::new(vec![0, 1, 2, 3]);
    let clone1 = bytes.split_to(0);
    assert_eq!(clone1.as_ptr(), bytes.as_ptr());
    let clone2 = bytes.split_off(bytes.len());
    assert_eq!(clone2.as_ptr(), bytes[bytes.len()..].as_ptr());
    let clone3 = bytes.subslice(1..1);
    assert_eq!(clone3.as_ptr(), bytes[1..].as_ptr());
    let mut clone4 = bytes.split_off(0);
    assert_eq!(bytes.as_ptr(), bytes[..].as_ptr());
    mem::swap(&mut clone4, &mut bytes);
    let mut clone5 = bytes.split_to(bytes.len());
    assert_eq!(bytes.as_ptr(), bytes[..].as_ptr());
    mem::swap(&mut clone5, &mut bytes);
    assert_eq!(bytes.downcast_buffer::<Vec<u8>>().unwrap(), [0, 1, 2, 3]);
}

// into_vec reuse the internal vector even if in subslice case
#[test]
fn into_vec() {
    let vec = vec![0, 1, 2, 3];
    let vec_ptr = vec.as_ptr();

    let bytes = <ArcBytes>::new(vec);
    let vec = bytes.into_vec();
    assert_eq!(vec.as_ptr(), vec_ptr);

    let mut bytes = <ArcBytes>::new(vec);
    bytes.advance(2);
    let vec = bytes.into_vec();
    assert_eq!(vec, [2, 3]);
    assert_eq!(vec.as_ptr(), vec_ptr);

    let mut bytes = <ArcBytes>::new(vec);
    bytes.truncate(1);
    let vec = bytes.into_vec();
    assert_eq!(vec, [2]);
    assert_eq!(vec.as_ptr(), vec_ptr);
}

#[derive(Default, Clone)]
struct Metadata {
    dropped: Arc<AtomicBool>,
}

impl Drop for Metadata {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Relaxed);
    }
}

// metadata can be downcasted, and is dropped when the slice is dropped
#[test]
fn metadata() {
    let metadata = Metadata::default();
    let bytes = <ArcBytes>::new_with_metadata(vec![42], metadata.clone());
    assert!(bytes.get_metadata::<()>().is_none());
    assert!(bytes.get_metadata::<Metadata>().is_some());

    let clone = bytes.clone();
    assert!(clone.get_metadata::<()>().is_none());
    assert!(clone.get_metadata::<Metadata>().is_some());

    assert!(ptr::eq(
        bytes.get_metadata::<Metadata>().unwrap(),
        clone.get_metadata::<Metadata>().unwrap()
    ));

    assert!(!metadata.dropped.load(Ordering::Relaxed));
    drop(bytes);
    drop(clone);
    assert!(metadata.dropped.load(Ordering::Relaxed));
}

// static/vec/inlined has a unit metadata that should stay the same even after clone
#[test]
fn unit_metadata() {
    let bytes = <ArcBytes>::new_static(&[]);
    assert!(ptr::eq(
        bytes.get_metadata::<()>().unwrap(),
        bytes.clone().get_metadata::<()>().unwrap()
    ));
    let bytes = <ArcBytes>::new(vec![]);
    assert!(ptr::eq(
        bytes.get_metadata::<()>().unwrap(),
        bytes.clone().get_metadata::<()>().unwrap()
    ));
}

// buffer cannot be downcasted if there are clones or if it is a subslice
#[test]
fn downcast_buffer() {
    let array = [0u8, 1, 2, 3];
    let bytes = <ArcBytes>::new(array);
    assert_eq!(bytes.downcast_buffer::<[u8; 4]>().unwrap(), [0, 1, 2, 3]);

    let bytes = <ArcBytes>::new(array);
    let clone = bytes.clone();
    assert!(bytes.downcast_buffer::<[u8; 4]>().is_err());
    assert_eq!(clone.downcast_buffer::<[u8; 4]>().unwrap(), [0, 1, 2, 3]);

    let mut bytes = <ArcBytes>::new(array);
    bytes.truncate(2);
    assert!(bytes.downcast_buffer::<[u8; 4]>().is_err());

    let mut bytes = <ArcBytes>::new(array);
    bytes.advance(2);
    assert!(bytes.downcast_buffer::<[u8; 4]>().is_err());
}

// vec cannot be downcasted if there are clones, but it works with subslices
#[test]
fn downcast_vec() {
    let bytes = <ArcBytes>::new(vec![42]);
    assert_eq!(bytes.downcast_buffer::<Vec<u8>>().unwrap(), [42]);

    let bytes = <ArcBytes>::new(vec![42]);
    let clone = bytes.clone();
    assert!(bytes.downcast_buffer::<Vec<u8>>().is_err());
    assert_eq!(clone.downcast_buffer::<Vec<u8>>().unwrap(), [42]);

    let mut bytes = <ArcBytes>::new(vec![0, 1, 2, 3]);
    bytes.truncate(2);
    assert_eq!(bytes.downcast_buffer::<Vec<u8>>().unwrap(), [0, 1]);

    let mut bytes = <ArcBytes>::new(vec![0, 1, 2, 3]);
    bytes.advance(2);
    assert_eq!(bytes.downcast_buffer::<Vec<u8>>().unwrap(), [2, 3]);
}

// static slices can always be downcasted
#[test]
fn downcast_static() {
    let bytes = <ArcBytes>::new_static(&[0, 1, 2, 3]);
    let subslice = bytes.subslice(..2);
    assert_eq!(subslice.downcast_buffer::<&'static [u8]>().unwrap(), [0, 1]);
    assert_eq!(
        bytes.downcast_buffer::<&'static [u8]>().unwrap(),
        [0, 1, 2, 3]
    );
}

// `new_with_metadata` with unit metadata is like `new`, so a static subslice can be downcasted
// it would not be the case if unit metadata was not handled specially
#[test]
fn downcast_static_with_unit_metadata() {
    let bytes = <ArcBytes>::new_with_metadata(<&'static [u8]>::from(&[0, 1, 2, 3]), ());
    let subslice = bytes.subslice(..2);
    assert_eq!(subslice.downcast_buffer::<&'static [u8]>().unwrap(), [0, 1]);
}

// ensure the metadata is dropped when the slice is downcasted
#[test]
fn downcast_buffer_with_metadata() {
    let metadata = Metadata::default();
    let bytes = <ArcBytes>::new_with_metadata(vec![42], metadata.clone());
    let _ = bytes.downcast_buffer::<Vec<u8>>().unwrap();
    assert!(metadata.dropped.load(Ordering::Relaxed));
}

#[test]
fn try_into_mut() {
    let bytes = <ArcBytes>::new(vec![42]);
    bytes.try_into_mut().unwrap();
}

#[test]
fn inlined() {
    let mut bytes = <ArcBytes<Compact>>::new([0, 1, 2, 3]);
    assert_eq!(bytes, [0, 1, 2, 3]);
    assert_eq!(bytes.split_off(2), [2, 3]);
    assert_eq!(bytes, [0, 1]);
}
