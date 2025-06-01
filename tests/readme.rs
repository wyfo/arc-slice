#[test]
fn readme_example() {
    use arc_slice::ArcSlice;

    let mut bytes = <ArcSlice<[u8]>>::from_slice(b"Hello world");
    let a = bytes.subslice(0..5);

    assert_eq!(a, b"Hello");

    let b = bytes.split_to(6);

    assert_eq!(bytes, b"world");
    assert_eq!(b, b"Hello ");
}

#[cfg(feature = "default-layout-any-buffer")]
#[test]
fn readme_example_memmap() -> std::io::Result<()> {
    use std::{
        fs::File,
        path::{Path, PathBuf},
    };

    use arc_slice::{buffer::AsRefBuffer, ArcBytes};
    use memmap2::Mmap;

    let path = Path::new("README.md");
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    let bytes = <ArcBytes>::from_buffer_with_metadata(AsRefBuffer(mmap), path.to_owned());

    assert_eq!(bytes.metadata::<PathBuf>().unwrap(), path);
    Ok(())
}
