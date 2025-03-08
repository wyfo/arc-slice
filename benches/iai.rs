#![allow(clippy::incompatible_msrv)]
use std::hint::black_box;

use arc_slice::ArcBytes;
use bytes::Bytes;
use iai_callgrind::{library_benchmark, library_benchmark_group, main};

#[library_benchmark]
fn arcslice_declare() {
    black_box(<ArcBytes>::new(vec![0u8; 8]));
}

#[library_benchmark]
fn arcslice_clone() {
    let bytes = <ArcBytes>::new(vec![0u8; 8]);
    black_box(black_box(&bytes).clone());
}

#[library_benchmark]
fn arcslice_100_clone() {
    let bytes = <ArcBytes>::new(vec![0u8; 8]);
    for _ in 0..100 {
        black_box(black_box(&bytes).clone());
    }
}

#[library_benchmark]
fn arcslice_subslice_and_split() {
    let mut bytes = <ArcBytes>::new(b"Hello world");
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
    let a = black_box(&bytes).slice(0..5);

    assert_eq!(black_box(&a), "Hello");

    let b = black_box(&mut bytes).split_to(6);

    assert_eq!(black_box(&bytes), "world");
    assert_eq!(black_box(&b), "Hello ");
}

library_benchmark_group!(name = bench_arcslice; benchmarks = arcslice_declare, arcslice_clone, arcslice_100_clone, arcslice_subslice_and_split);
library_benchmark_group!(name = bench_bytes; benchmarks = bytes_declare, bytes_clone, bytes_100_clone, bytes_subslice_and_split);
main!(library_benchmark_groups = bench_arcslice, bench_bytes);
