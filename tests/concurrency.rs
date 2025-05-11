use std::{sync::Arc, thread};

use arc_slice::{layout::BoxedSliceLayout, ArcBytes};

#[test]
fn arc_slice_vec_concurrent_clone() {
    let bytes = Arc::new(ArcBytes::<BoxedSliceLayout>::from(vec![42]));
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
