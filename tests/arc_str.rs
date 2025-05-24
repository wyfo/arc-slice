use arc_slice::{layout::ArcLayout, ArcStr};

#[test]
fn try_into_buffer() {
    let bytes = ArcStr::<ArcLayout<true, true>>::from("plop".to_string());
    assert_eq!(bytes.try_into_buffer::<String>().unwrap(), "plop");

    let bytes = ArcStr::<ArcLayout<true, true>>::from_static("plop");
    assert_eq!(bytes.try_into_buffer::<&'static str>().unwrap(), "plop");
}
