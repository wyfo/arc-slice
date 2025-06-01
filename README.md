# arc-slice

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](
https://github.com/wyfo/arc-slice/blob/main/LICENSE)
[![Cargo](https://img.shields.io/crates/v/arc-slice.svg)](
https://crates.io/crates/arc-slice)
[![Documentation](https://docs.rs/arc-slice/badge.svg)](
https://docs.rs/arc-slice)

A utility library for working with shared slices of memory.

## Example

```rust
use arc_slice::ArcSlice;

let mut bytes = <ArcSlice<u8>>::new(b"Hello world");
let a = bytes.subslice(0..5);

assert_eq!(a, b"Hello");

let b = bytes.split_to(6);

assert_eq!(bytes, b"world");
assert_eq!(b, b"Hello ");
```

Using `arc-slice` with shared memory:
```rust
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
```

## *Disclaimer*

This library is at early stage of development. It lacks a proper documentation, deeper tests, and most of the safety comments. However, it is still tested with [loom](https://crates.io/crates/loom) and [miri](https://github.com/rust-lang/miri), and passes the entire `bytes` test suite[^1] with miri, so it should be reasonably safe to use.

## Difference with [`bytes`](https://crates.io/crates/bytes)

`arc-slice` is of course a lot inspired by `bytes`, with the same core features. However, it aims to be more generic, with a quite different implementation. Here is a non-exhaustive list of the differences:

- `ArcSlice<T>`/`ArcSliceMut<T>` are generic over the slice item type. You would still mostly use bytes slices, that's why `ArcBytes`/`ArcBytesMut` aliases are provided for `T=u8`.
- Immutable string slices are also supported through a `ArcStr` type. Although it is possible to use [`string`](https://crates.io/crates/string) crate with `string::String<bytes::Bytes>`, it isn't as handy as a dedicated `ArcStr`, as the former doesn't support subslice/advance operations.
- `ArcSlice` uses only 3 words in memory, while `bytes::Bytes` use 4 words.
- More precisely `ArcSlice<T, L=Compact>` has a generic layout. The default one named `Compact` uses 3 words and has to allocate an Arc when storing a vector not full. The other one `Plain` uses 4 words and doesn't have to allocate an Arc for any vector. (Of course, an Arc is allocated with both layouts when the slice is cloned)
- The generic layout has the negative ergonomic impact regarding type inference, as constructor cannot always infer the layout, requiring to write `<ArcSlice>::new` instead of `ArcSlice::new`. However, in real projects where the layout is specified in function signatures, inference should succeed.
- Both `ArcSlice` and `ArcSliceMut` supports arbitrary buffer type, as long as they implement the required `Buffer`/`BufferMut` trait. On the other hand, only `bytes::Bytes` supports arbitrary buffer types.[^2]
- As a consequence, `ArcSliceMut::try_reserve` is fallible, as the reservation operation may not be supported by the underlying buffer.[^3] Also, contrary to `bytes::BytesMut`, there is no implicit reallocation + copy when the slice is actually shared; every copy is made explicit.
- Arbitrary buffers can be associated with arbitrary metadata, and metadata can be accessed anytime. It can be used for example to store a shared memory name associated to a shared memory buffer.
- `ArcSlice`/`ArcSliceMut` can be downcasted to the original buffer.
- `ArcSliceRef` allows to reference a subslice of an `ArcSlice` without cloning it. This is roughly the same as `(&[T], &ArcSlice<T>)`, but with a more explicit semantic, and no need to repeat the bounds check if promoted to an `ArcSlice` clone.
- `ArcSlice` and `ArcStr` are compatible with [small string optimization](https://cppdepend.com/blog/understanding-small-string-optimization-sso-in-stdstring/). The `inlined` feature adds `SmallArcSlice`/`SmallArcStr`, which can avoid allocations when slices are small enough.
- `bytes::Bytes` is implemented using a vtable mixed with pointer tagging, while `ArcSlice` mostly use pointer tagging, and a vtable only for arbitrary buffers/metadata. That's how `ArcSlice` is able to use one less word in memory.
- Because of the implementation difference, `arc-slice` code is more inlinable than `bytes` one. But it trades function pointer call off for a bit  more conditional jumps. In micro-benchmarks, the performance seems to be a bit better with `arc-slice`, but it may depend on the use case. Still, `ArcSlice` uses one less word than `bytes::Bytes`, and that may not be negligible.   

## Compatibility with `bytes`

This library is not compatible with bytes, as their internal implementation are different and not exposed. However, `bytes::Bytes`/`bytes::BytesMut` can be fully implemented using `arc-slice`, with all the test suite passing[^1].

This repository provides in fact a drop-in replacement for `bytes`, that can used as simply as adding these lines in `Cargo.toml`/cargo [config file](https://doc.rust-lang.org/cargo/reference/config.html):
```
[patch.crates-io]
bytes = { git = "https://github.com/wyfo/arc-slice.git" }
```

It can allow to test `arc-slice` implementation, to check if it can perform better than `bytes` for a given use case. Also, this patched `bytes` crate is fully compatible with `arc-slice`, so `Bytes` can be converted to/from `ArcBytes`, and it can benefit from `arc-slice` exclusive features, like buffer metadata.

## Safety

This library uses unsafe code. It is tested with [miri](https://github.com/rust-lang/miri) to ensure the memory safety and the correct synchronization.


[^1]: Only two tests are not passing, but it is just about the capacity of a reallocated `BytesMut` for which you cannot reserve because it is shared. I don't really agree with `bytes` behavior here — doubling the previous capacity, even if it's about a small reservation in a small subslice — so I will not say it matters a lot. If it does matter, it is still possible to match `bytes` behavior. 

[^2]: At the time when I started to draft this crate, during summer 2024, `Bytes::from_owner` didn't exist, https://github.com/tokio-rs/bytes/issues/437 was a bit staled, and that was actually my first motivation to start this work.

[^3]: In earlier version, `try_reserve` also exposed memory allocation failure in the error, but it has been simplified. I'm still thinking about a design where all memory allocation failures, including arc allocation, would be exposed, but it's not ready yet.