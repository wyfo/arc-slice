use std::hint::black_box;

use arc_slice::ArcBytes;
use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion};

// fn empty(c: &mut Criterion) {
//     let mut group = c.benchmark_group("empty");
//     group.bench_function("arcslice", |b| {
//         b.iter(<ArcBytes>::default);
//     });
//     group.bench_function("bytes", |b| {
//         b.iter(Bytes::default);
//     });
// }
//
// fn clone_vec(c: &mut Criterion) {
//     let mut group = c.benchmark_group("clone_vec");
//     group.bench_function("arcslice", |b| {
//         b.iter_batched(
//             || <ArcBytes>::from(vec![0u8; 8]),
//             |bytes| bytes.clone(),
//             BatchSize::SmallInput,
//         );
//     });
//     group.bench_function("bytes", |b| {
//         b.iter_batched(
//             || Bytes::from(vec![0u8; 8]),
//             |bytes| bytes.clone(),
//             BatchSize::SmallInput,
//         );
//     });
// }
//
// fn clone_static(c: &mut Criterion) {
//     let mut group = c.benchmark_group("clone_static");
//     group.bench_function("arcslice", |b| {
//         let bytes = <ArcBytes>::new_static(&[]);
//         b.iter(|| bytes.clone());
//     });
//     group.bench_function("bytes", |b| {
//         let bytes = Bytes::from_static(&[]);
//         b.iter(|| bytes.clone());
//     });
// }
//
// fn clone_shared(c: &mut Criterion) {
//     let mut group = c.benchmark_group("clone_shared");
//     group.bench_function("arcslice", |b| {
//         let bytes = <ArcBytes>::from(vec![0u8; 8]);
//         let _ = bytes.clone();
//         b.iter(|| bytes.clone());
//     });
//     group.bench_function("bytes", |b| {
//         let bytes = Bytes::from(vec![0u8; 8]);
//         let _ = bytes.clone();
//         b.iter(|| bytes.clone());
//     });
// }

fn subslice_and_split(c: &mut Criterion) {
    let mut group = c.benchmark_group("subslice_and_split");
    group.bench_function("arcslice", |b| {
        b.iter(|| {
            let mut bytes = <ArcBytes>::from_slice(b"Hello world");
            let a = bytes.subslice(0..5);

            assert_eq!(a, b"Hello");

            let b = bytes.split_to(6);

            assert_eq!(bytes, b"world");
            assert_eq!(b, b"Hello ");
        });
    });
    group.bench_function("bytes", |b| {
        b.iter(|| {
            let mut bytes = Bytes::copy_from_slice(b"Hello world");
            let a = bytes.slice(0..5);

            assert_eq!(a, "Hello");

            let b = bytes.split_to(6);

            assert_eq!(bytes, "world");
            assert_eq!(b, "Hello ");
        });
    });
}

fn subslice_and_split_black_box(c: &mut Criterion) {
    let mut group = c.benchmark_group("subslice_and_split_black_box");
    group.bench_function("arcslice", |b| {
        b.iter(|| {
            let mut bytes = <ArcBytes>::from_slice(b"Hello world");
            let a = black_box(&bytes).subslice(0..5);

            assert_eq!(black_box(&a), b"Hello");

            let b = black_box(&mut bytes).split_to(6);

            assert_eq!(black_box(&bytes), b"world");
            assert_eq!(black_box(&b), b"Hello ");
        });
    });
    group.bench_function("bytes", |b| {
        b.iter(|| {
            let mut bytes = Bytes::copy_from_slice(b"Hello world");
            let a = black_box(&bytes).slice(0..5);

            assert_eq!(black_box(&a), "Hello");

            let b = black_box(&mut bytes).split_to(6);

            assert_eq!(black_box(&bytes), "world");
            assert_eq!(black_box(&b), "Hello ");
        });
    });
}
criterion_group!(
    benches,
    // empty,
    // clone_vec,
    // clone_static,
    // clone_shared,
    subslice_and_split,
    subslice_and_split_black_box,
);
criterion_main!(benches);
