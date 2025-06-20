use std::hint::black_box;

use arc_slice::{layout::ArcLayout, ArcBytes};
use bytes::Bytes;
use iai_callgrind::{library_benchmark, library_benchmark_group, main};

#[library_benchmark]
fn arcslice_declare() {
    black_box(<ArcBytes<ArcLayout<true>>>::from(b"hello world"));
}

#[library_benchmark]
fn arcslice_declare_drop_unique() {
    black_box(<ArcBytes<ArcLayout<true>>>::from(b"hello world")).drop_with_unique_hint();
}

#[library_benchmark]
fn arcslice_clone() {
    let bytes = <ArcBytes<ArcLayout<true>>>::from(b"hello world");
    black_box(black_box(&bytes).clone());
}

#[library_benchmark]
fn arcslice_100_clone() {
    let bytes = <ArcBytes<ArcLayout<true>>>::from(b"hello world");
    for _ in 0..100 {
        black_box(black_box(&bytes).clone());
    }
}

#[library_benchmark]
fn arcslice_subslice_and_split() {
    let mut bytes = <ArcBytes<ArcLayout<false, true>>>::from_static(b"hello world");
    let a = bytes.subslice(0..5);

    assert_eq!(a, b"hello");

    let b = bytes.split_to(6);

    assert_eq!(bytes, b"world");
    assert_eq!(b, b"hello ");
}

#[library_benchmark]
fn arcslice_subslice_and_split_black_box() {
    let mut bytes = <ArcBytes<ArcLayout<false, true>>>::from_slice(b"hello world");
    let a = black_box(&bytes).subslice(0..5);

    assert_eq!(black_box(&a), b"hello");

    let b = black_box(&mut bytes).split_to(6);

    assert_eq!(black_box(&bytes), b"world");
    assert_eq!(black_box(&b), b"hello ");
}

#[library_benchmark]
fn bytes_declare() {
    black_box(Bytes::copy_from_slice(b"hello world"));
}

#[library_benchmark]
fn bytes_clone() {
    let bytes = Bytes::copy_from_slice(b"hello world");
    black_box(black_box(&bytes).clone());
}

#[library_benchmark]
fn bytes_100_clone() {
    let bytes = Bytes::copy_from_slice(b"hello world");
    for _ in 0..100 {
        black_box(black_box(&bytes).clone());
    }
}

#[library_benchmark]
fn bytes_subslice_and_split() {
    let mut bytes = Bytes::from_static(b"hello world");
    let a = bytes.slice(0..5);

    assert_eq!(a, "hello");

    let b = bytes.split_to(6);

    assert_eq!(bytes, "world");
    assert_eq!(b, "hello ");
}

#[library_benchmark]
fn bytes_subslice_and_split_black_box() {
    let mut bytes = Bytes::from_static(b"hello world");
    let a = black_box(&bytes).slice(0..5);

    assert_eq!(black_box(&a), "hello");

    let b = black_box(&mut bytes).split_to(6);

    assert_eq!(black_box(&bytes), "world");
    assert_eq!(black_box(&b), "hello ");
}

library_benchmark_group!(name = bench_arcslice; benchmarks = arcslice_declare, arcslice_declare_drop_unique, arcslice_clone, arcslice_100_clone, arcslice_subslice_and_split, arcslice_subslice_and_split_black_box);
library_benchmark_group!(name = bench_bytes; benchmarks = bytes_declare, bytes_clone, bytes_100_clone, bytes_subslice_and_split, bytes_subslice_and_split_black_box);
main!(library_benchmark_groups = bench_arcslice, bench_bytes);
