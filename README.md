# arc-slice

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](
https://github.com/wyfo/arc-slice/blob/main/LICENSE)
[![Cargo](https://img.shields.io/crates/v/arc-slice.svg)](
https://crates.io/crates/arc-slice)
[![Documentation](https://docs.rs/arc-slice/badge.svg)](
https://docs.rs/arc-slice)

A utility library for working with shared slices of memory.

## ⚠️ I HAVE A BIG REWORK IN PROGRESS ⚠️

More optimizations (less instructions, better predictability), more features (custom `Arc` support without additional allocation, unique mutable slice, etc.), more generic interface (slice trait, more layouts, default layout selection with features, etc.), more no_std-friendly (fallible allocations), more documentation (-_-'), etc.

Stay tuned! 

## Example

```rust
use arc_slice::{ArcSlice, ArcSliceMut};

let mut bytes_mut: ArcSliceMut<[u8]> = ArcSliceMut::new();
bytes_mut.extend_from_slice(b"Hello world");

let mut bytes: ArcSlice<[u8]> = bytes_mut.freeze();

let a: ArcSlice<[u8]> = bytes.subslice(0..5);
assert_eq!(a, b"Hello");

let b: ArcSlice<[u8]> = bytes.split_to(6);
assert_eq!(bytes, b"world");
assert_eq!(b, b"Hello ");
```

## Difference with [`bytes`](https://crates.io/crates/bytes)

`arc-slice` shares the same core features and scope as `bytes`. However, it aims to be more generic and performant, while offering more features. Its internal implementation is also significantly different. Here is a non-exhaustive list of the differences: 

#### Genericity

`ArcSlice`/`ArcSliceMut` are generic over the slice type, so you can use `ArcSlice<[u8]>` (aliased to `ArcBytes`), `ArcSlice<str>` (aliased to `ArcStr`), or any other specific slice type you may need. 
<br>
They also support multiple [layouts](#layouts), passed as a generic parameter. A layout defines how the data is stored, impacting memory size and the behavior of some operations like `clone`.

#### Uniqueness

`ArcSliceMut` has an additional `UNIQUE` generic boolean parameter, that indicates if it is the only instance referencing its data. If `UNIQUE=true`, every mutation that would involve a uniqueness check, such as `reserve` or `drop`, can skip this check.
<br>
Moreover, `ArcSlice::drop_with_unique_hint` can leverage the uniqueness hint to use an implementation shortcut.

#### Inner buffer

While `bytes::Bytes` is roughly equivalent to an `Arc<Vec<u8>>`, `ArcBytes` default buffer implementation can be thought of more like an `Arc<[u8]>`: bytes are directly written into the memory block allocated for the Arc, so it requires one fewer allocation/deallocation.

#### Arbitrary buffer and metadata

Both `ArcSlice` and `ArcSliceMut` can wrap arbitrary buffers, while only `bytes::Bytes` supports it[^2]. Arbitrary metadata can also be attached for contextual or domain-specific needs, and can be retrieved at any time. Furthermore, `ArcSlice`/`ArcSliceMut` can be downcast to the wrapped buffer.
<br>
When no buffer is provided, e.g. `ArcSlice::from_slice`, the [default buffer implementation](#inner-buffer) is used.

#### [Layouts](https://docs.rs/arc-slice/latest/arc_slice/layout/index.html)

`ArcSlice` supports 4 layouts:
- `ArcLayout`: The default and most *optimized* layout, which aims to be more performant than the others for supported operations, though other layouts may support a broader range of use cases. It can be customized through generic parameters.
- `BoxedSliceLayout`: Enables storing a boxed slice into an `ArcSlice` without requiring the allocation of an inner Arc, as long as there is a single instance.
- `VecLayout`: Enables storing a vector into an `ArcSlice` without requiring the allocation of an inner Arc, as long as there is a single instance.
- `RawLayout` Enables storing a [raw buffer](#raw-buffer), without requiring the allocation of an inner Arc.

All layouts are compatible and can cheaply be converted to each other. Both `ArcSlice` and `ArcSliceMut` have a default layout, which can be modified using [compilation features](https://docs.rs/arc-slice/latest/arc_slice/layout/index.html#features).

Here is a summary, see layout [documentation](https://docs.rs/arc-slice/latest/arc_slice/layout/index.html) for more details:
| Layout             | `ArcSlice` size          | static/empty slices support | arbitrary buffers support | cloning may allocate | optimized for      |
|--------------------|--------------------------|-----------------------------|---------------------------|-----------------------|--------------------|
| `ArcLayout`        | `3 * size_of::<usize>()` | yes (optional)              | yes (optional)            | no                    | regular `ArcSlice` |
| `BoxedSliceLayout` | `3 * size_of::<usize>()` | yes                         | yes                       | yes                   | `Box<[T]>`         |
| `VecLayout`        | `4 * size_of::<usize>()` | yes                         | yes                       | yes                   | `Vec<T>`           |
| `RawLayout`        | `4 * size_of::<usize>()` | yes                         | yes                       | no                    | `RawBuffer`        |

On the other hand, `bytes::Bytes` uses a vtable-based implementation — so it takes 4 words in memory, but allows to store `Box<[u8]>` without allocating an Arc as long as it is not cloned. So it's kind of a mix between `RawLayout`, which also stores a vtable along the inner Arc, and `BoxedSliceLayout`[^3].

The generic layout design can negatively affect type inference, as constructors cannot always infer the layout. However, in real projects, layout (or its default value) are most often specified in function signatures, so inference usually works.
<br>
While having multiple layouts brings additional complexity, the [performance improvement](#performances) justifies it, especially for such a fundamental crate.

#### Raw buffer

Some buffers are already reference counted, for example when they are embedded inside an `Arc`. If in addition, they can be stored on a single pointer, which is the case for `Arc`, then they can be wrapped directly into an `ArcSlice` without further inner Arc allocation if `RawLayout` is used.

#### Borrowed view

`ArcSliceBorrow` gives a borrowed view to an `ArcSlice`'s subslice, which can then be turned into a new `ArcSlice`. It has more explicit semantics than for example a pair `(&[u8], &ArcBytes)`, and `ArcSliceBorrow::clone_arc` avoids the redundant bound check you would have by turning the `&[u8]` into a new `ArcBytes` with `ArcBytes::subslice_from_ref`. `ArcSliceBorrow` also has more optimized implementation than `(&[u8], &ArcBytes)`.

#### No implicit reallocations

`bytes::BytesMut` operations always reallocate and copy data when an operation cannot be performed on the shared data.
There is no implicit reallocation in `arc-slice`, and [uniqueness](#uniqueness) helps to ensure that operations can always be performed.

#### Fallible allocations + global OOM handling

Each method that may perform an allocation has a fallible `try_`-prefixed counterpart. Methods that rely on the global OOM handler require the `oom-handling` compilation feature (enabled by default), which can be disabled to ensure that every allocation is explicitly checked.

#### Reference counting saturation

Standard `Arc` as well as `bytes` types abort in case of reference counting overflow. However, this behavior is not always suitable, and another way of handling overflow is by saturating the reference counter, leading to an effective leak. This behavior is used in Linux reference counting, and is implemented by `arc-slice`. The `abort-on-refcount-overflow` feature (enabled by default) replace saturation with aborting. 

#### Small string optimization

`ArcSlice` is compatible with [small string optimization](https://cppdepend.com/blog/understanding-small-string-optimization-sso-in-stdstring/). The `inlined` feature exposes `SmallArcSlice` type, which can avoid allocation by storing small data inlined.

#### Performances

`arc-slice` offers a more configurable API than `bytes` with its [layouts](#layouts), in order to offer the best performance possible. However, the flexibility of layouts introduces some runtime cost. This cost might often be negligible, but a low-level crate such as this one should allow users to not be impacted by features they don't use.

Examples of additional cost are:
- supporting static slices without an Arc allocation means an additional branching in each clone and drop
- arbitrary buffer support is also an additional branching and virtual call in drop
- `VecLayout` and `RawLayout` uses 4 words of memory, compared to 3 for others
- Arc allocation-on-clone for `BoxedSliceLayout`/`VecLayout` means using atomic pointer and an *acquire* load for clone operation
- raw buffer support adds a virtual method call in clone

On the other hand, `bytes::Bytes` always use virtual method call for clone/drop, atomic pointer for instances initialized with a `Vec<u8>`/`Box<[u8]>`, takes 4 words in memory, and does not store data directly in the allocated Arc. 
<br>
There is an obvious theoretical performance advantage for `ArcSlice` with its default `ArcLayout`, especially coming from better inlining (no virtual call) and fewer allocations. Benchmark results appear to confirm this advantage.

Here are the results of the own `bytes` benchmark, compared to the exact same benchmark using an `arc-slice`-based implementation [100% compatible with `bytes`](#compatibility-with-bytes):
| benchmark              | `bytes`           | `arc-slice`       |
|------------------------|-------------------|-------------------|
| `clone_arc_vec`        | 3,629.94 ns/iter  | 3,634.34 ns/iter  |
| `clone_shared`         | 12,635.02 ns/iter | 3,972.39 ns/iter  |
| `clone_static`         | 9,496.43 ns/iter  | 1,026.70 ns/iter  |
| `deref_shared`         | 517.27 ns/iter    | 517.27 ns/iter    |
| `deref_static`         | 513.31 ns/iter    | 517.34 ns/iter    |
| `deref_unique`         | 517.35 ns/iter    | 517.25 ns/iter    |
| `from_long_slice`      | 17.04 ns/iter     | 13.28 ns/iter     |
| `slice_empty`          | 9,056.49 ns/iter  | 1,253.79 ns/iter  |
| `slice_short_from_arc` | 11,603.41 ns/iter | 3,236.20 ns/iter  |
| `split_off_and_drop`   | 40,735.66 ns/iter | 30,946.47 ns/iter |

*The `arc-slice` benchmark uses `ArcLayout<true, true>` (because of `bytes` compatibility), so results could be even better with `ArcLayout<false, false>`.*

Of course, the performance difference may vary depending on the real use cases.
 
## Compatibility with `bytes`

This library is not compatible with bytes, as their internal implementations are different and not exposed. However, `bytes::Bytes`/`bytes::BytesMut` can be fully implemented using `arc-slice`, with all the test suite passing[^1].

This repository provides in fact a drop-in replacement for `bytes`, that can be used simply by adding these lines in `Cargo.toml` or cargo [config file](https://doc.rust-lang.org/cargo/reference/config.html):
```
[patch.crates-io]
bytes = { git = "https://github.com/wyfo/arc-slice.git" }
```

It allows testing `arc-slice` implementation, to evaluate whether it outperforms bytes in specific use cases. Also, this patched `bytes` crate is fully compatible with `arc-slice`, so `Bytes` can be converted to/from `ArcBytes`, and it can benefit from `arc-slice`'s exclusive features such as buffer metadata.

## Safety

This library uses unsafe code. It is tested with [miri](https://github.com/rust-lang/miri), including the entire `bytes` test suite[^1], to ensure memory safety and correct synchronization.


[^1]: Only four tests fails to pass: one about the memory size being 3 instead of 4, another one fails because `BytesMut::with_capacity` does not allocate a `Vec`, and the last two about the capacity of a reallocated `BytesMut` for which you cannot reserve because it is shared. `bytes` behavior may be debatable — doubling the previous capacity, even if it's about a small reservation in a small subslice, seems questionable — but this case is rare so it should not matter a lot. If it does matter, it is still possible to match `bytes` behavior. 

[^2]: At the time when I started to draft this crate, during summer 2024, `Bytes::from_owner` didn't exist, https://github.com/tokio-rs/bytes/issues/437 was a bit staled, which initially motivated the creation of this crate.

[^3]: I could have implemented the same layout as `bytes::Bytes`, i.e. a mix between `RawLayout` and `BoxedSliceLayout`, and actually I started it. But it introduced too much complexity, and I was not convinced of the added value, so I have preferred to not include it.