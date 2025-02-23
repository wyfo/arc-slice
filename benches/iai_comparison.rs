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
fn bytes_declare() {
    black_box(Bytes::from(vec![0u8; 8]));
}

#[library_benchmark]
fn bytes_clone() {
    let bytes = Bytes::from(vec![0u8; 8]);
    black_box(black_box(&bytes).clone());
}

library_benchmark_group!(name = bench_arcslice; benchmarks = arcslice_declare, arcslice_clone);
library_benchmark_group!(name = bench_bytes; benchmarks = bytes_declare, bytes_clone);
main!(library_benchmark_groups = bench_arcslice, bench_bytes);
