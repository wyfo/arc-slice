use std::hint::black_box;

use arc_slice::{layout::ArcLayout, ArcBytes};
use bytes::Bytes;
use iai_callgrind::{library_benchmark, library_benchmark_group, main};

#[library_benchmark]
fn arcslice_declare() {
    black_box(<ArcBytes<ArcLayout<true>>>::from(vec![0u8; 8]));
}

#[library_benchmark]
fn arcslice_clone() {
    let bytes = <ArcBytes<ArcLayout<true>>>::from(vec![0u8; 8]);
    black_box(black_box(&bytes).clone());
}

#[library_benchmark]
fn arcslice_100_clone() {
    let bytes = <ArcBytes<ArcLayout<true>>>::from(vec![0u8; 8]);
    for _ in 0..100 {
        black_box(black_box(&bytes).clone());
    }
}

#[library_benchmark]
fn arcslice_subslice_and_split() {
    let mut bytes = <ArcBytes<ArcLayout<false, true>>>::from_static(b"Hello world");
    let a = bytes.subslice(0..5);

    assert_eq!(a, b"Hello");

    let b = bytes.split_to(6);

    assert_eq!(bytes, b"world");
    assert_eq!(b, b"Hello ");
}

#[library_benchmark]
fn arcslice_subslice_and_split_black_box() {
    let mut bytes = <ArcBytes<ArcLayout<false, true>>>::from_slice(b"Hello world");
    let a = black_box(&bytes).subslice(0..5);

    assert_eq!(black_box(&a), b"Hello");

    let b = black_box(&mut bytes).split_to(6);

    assert_eq!(black_box(&bytes), b"world");
    assert_eq!(black_box(&b), b"Hello ");
}

#[library_benchmark]
fn bytes_declare() {
    black_box(Bytes::from(vec![0u8; 8]));
}

#[library_benchmark]
fn bytes_clone() {
    let bytes = Bytes::from(vec![0u8; 8]);
    black_box(black_box(&bytes).clone());
}

#[library_benchmark]
fn bytes_100_clone() {
    let bytes = Bytes::from(vec![0u8; 8]);
    for _ in 0..100 {
        black_box(black_box(&bytes).clone());
    }
}

#[library_benchmark]
fn bytes_subslice_and_split() {
    let mut bytes = Bytes::from("Hello world");
    let a = bytes.slice(0..5);

    assert_eq!(a, "Hello");

    let b = bytes.split_to(6);

    assert_eq!(bytes, "world");
    assert_eq!(b, "Hello ");
}

#[library_benchmark]
fn bytes_subslice_and_split_black_box() {
    let mut bytes = Bytes::from("Hello world");
    let a = black_box(&bytes).slice(0..5);

    assert_eq!(black_box(&a), "Hello");

    let b = black_box(&mut bytes).split_to(6);

    assert_eq!(black_box(&bytes), "world");
    assert_eq!(black_box(&b), "Hello ");
}

library_benchmark_group!(name = bench_arcslice; benchmarks = arcslice_declare, arcslice_clone, arcslice_100_clone, arcslice_subslice_and_split, arcslice_subslice_and_split_black_box);
library_benchmark_group!(name = bench_bytes; benchmarks = bytes_declare, bytes_clone, bytes_100_clone, bytes_subslice_and_split, bytes_subslice_and_split_black_box);
main!(library_benchmark_groups = bench_arcslice, bench_bytes);
