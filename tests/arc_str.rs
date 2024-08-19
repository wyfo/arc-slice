use arc_slice::ArcStr;

#[test]
fn downcast_buffer() {
    let bytes = <ArcStr>::new("plop".to_string());
    assert_eq!(bytes.downcast_buffer::<String>().unwrap(), "plop");

    let bytes = <ArcStr>::new_static("plop");
    assert_eq!(bytes.downcast_buffer::<&'static str>().unwrap(), "plop");
}
