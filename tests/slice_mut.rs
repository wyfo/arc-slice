use arc_slice::{layout::VecLayout, ArcBytesMut};

#[test]
fn reclaim_vec() {
    let mut bytes = ArcBytesMut::<VecLayout>::from(Vec::with_capacity(1000));
    let ptr = bytes.as_ptr();
    bytes.extend(0..100);
    bytes.advance(100);
    bytes.reserve(1000);
    assert_eq!(bytes.as_ptr(), ptr);
}
