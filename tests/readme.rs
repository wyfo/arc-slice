#[test]
fn readme_example() {
    use arc_slice::ArcSlice;

    let mut bytes = <ArcSlice<[u8]>>::new(b"Hello world");
    let a = bytes.subslice(0..5);

    assert_eq!(a, b"Hello");

    let b = bytes.split_to(6);

    assert_eq!(bytes, b"world");
    assert_eq!(b, b"Hello ");
}

#[cfg(feature = "default-layout-any-buffer")]
#[test]
fn readme_example_shm() {
    use arc_slice::{
        buffer::{BorrowMetadata, Buffer},
        ArcBytes,
    };
    use shared_memory::{Shmem, ShmemConf};

    struct MyShmBuffer(Shmem);
    unsafe impl Send for MyShmBuffer {}
    unsafe impl Sync for MyShmBuffer {}
    impl Buffer<[u8]> for MyShmBuffer {
        fn as_slice(&self) -> &[u8] {
            unsafe { self.0.as_slice() }
        }
    }

    #[repr(transparent)]
    struct MyShmMetadata(Shmem);
    unsafe impl Sync for MyShmMetadata {}
    impl BorrowMetadata for MyShmBuffer {
        type Metadata = MyShmMetadata;
        fn borrow_metadata(&self) -> &Self::Metadata {
            unsafe { core::mem::transmute(&self.0) }
        }
    }

    let shmem = ShmemConf::new().size(8).create().unwrap();
    let os_id = shmem.get_os_id().to_owned();

    let bytes = <ArcBytes>::from_buffer_with_borrowed_metadata(MyShmBuffer(shmem));
    let metadata = bytes.metadata::<MyShmMetadata>().unwrap();
    assert_eq!(metadata.0.get_os_id(), os_id);
}
