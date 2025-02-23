use arc_slice::ArcBytes;
use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn empty(c: &mut Criterion) {
    let mut group = c.benchmark_group("empty");
    group.bench_function("arcslice", |b| {
        b.iter(<ArcBytes>::default);
    });
    group.bench_function("bytes", |b| {
        b.iter(Bytes::default);
    });
}

fn clone_vec(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone_vec");
    group.bench_function("arcslice", |b| {
        b.iter_batched(
            || <ArcBytes>::new(vec![0u8; 8]),
            |bytes| bytes.clone(),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("bytes", |b| {
        b.iter_batched(
            || Bytes::from(vec![0u8; 8]),
            |bytes| bytes.clone(),
            BatchSize::SmallInput,
        );
    });
}

fn clone_static(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone_static");
    group.bench_function("arcslice", |b| {
        let bytes = <ArcBytes>::new_static(&[]);
        b.iter(|| bytes.clone());
    });
    group.bench_function("bytes", |b| {
        let bytes = Bytes::from_static(&[]);
        b.iter(|| bytes.clone());
    });
}

fn clone_shared(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone_shared");
    group.bench_function("arcslice", |b| {
        let bytes = <ArcBytes>::new(vec![0u8; 8]).clone();
        b.iter(|| bytes.clone());
    });
    group.bench_function("bytes", |b| {
        let bytes = Bytes::from(vec![0u8; 8]).clone();
        b.iter(|| bytes.clone());
    });
}

criterion_group!(benches, empty, clone_vec, clone_static, clone_shared);
criterion_main!(benches);
