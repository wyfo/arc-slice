use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    any::Any,
    borrow::Borrow,
    cmp,
    convert::Infallible,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, RangeBounds},
    ptr::NonNull,
    str::FromStr,
};

#[cfg(feature = "raw-buffer")]
use crate::buffer::RawBuffer;
#[allow(unused_imports)]
use crate::msrv::ConstPtrExt;
#[allow(unused_imports)]
use crate::msrv::{ptr, NonNullExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{
        BorrowMetadata, Buffer, BufferExt, BufferMut, BufferWithMetadata, DynBuffer, Slice,
        SliceExt, Subsliceable,
    },
    layout::{AnyBufferLayout, DefaultLayout, FromLayout, Layout, LayoutMut, StaticLayout},
    macros::{assume, is},
    slice_mut::{ArcSliceMutLayout, Data},
    utils::{
        assert_checked, debug_slice, lower_hex, offset_len, offset_len_subslice,
        panic_out_of_range, try_transmute, upper_hex, UnwrapChecked,
    },
    ArcSliceMut,
};

mod arc;
#[cfg(feature = "raw-buffer")]
mod raw;
mod vec;

#[allow(clippy::missing_safety_doc)]
pub unsafe trait ArcSliceLayout: 'static {
    type Data;
    const ANY_BUFFER: bool = true;
    const STATIC_DATA: Option<Self::Data> = None;
    // MSRV 1.83 const `Option::unwrap`
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> = MaybeUninit::uninit();
    fn data_from_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(
        arc: Arc<S, ANY_BUFFER>,
    ) -> Self::Data;
    fn data_from_arc_slice<S: Slice + ?Sized>(arc: Arc<S, false>) -> Self::Data {
        Self::data_from_arc(arc)
    }
    fn data_from_arc_buffer<S: Slice + ?Sized, const ANY_BUFFER: bool, B: DynBuffer + Buffer<S>>(
        arc: Arc<S, ANY_BUFFER>,
    ) -> Self::Data {
        Self::data_from_arc(arc)
    }
    fn data_from_static<S: Slice + ?Sized>(_slice: &'static S) -> Self::Data {
        Self::STATIC_DATA.unwrap()
    }
    fn data_from_vec<S: Slice + ?Sized>(vec: S::Vec) -> Self::Data;
    #[cfg(feature = "raw-buffer")]
    fn data_from_raw_buffer<S: Slice + ?Sized, B: DynBuffer + RawBuffer<S>>(
        _buffer: *const (),
    ) -> Option<Self::Data> {
        None
    }
    fn clone<S: Slice + ?Sized>(
        start: NonNull<S::Item>,
        length: usize,
        data: &Self::Data,
    ) -> Self::Data;
    unsafe fn drop<S: Slice + ?Sized, const UNIQUE_HINT: bool>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    );
    fn borrowed_data<S: Slice + ?Sized>(_data: &Self::Data) -> Option<*const ()> {
        None
    }
    fn clone_borrowed_data<S: Slice + ?Sized>(_ptr: *const ()) -> Option<Self::Data> {
        None
    }
    fn truncate<S: Slice + ?Sized>(
        _start: NonNull<S::Item>,
        _length: usize,
        _data: &mut Self::Data,
    ) {
    }
    fn is_unique<S: Slice + ?Sized>(data: &Self::Data) -> bool;
    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Self::Data) -> Option<&M>;
    unsafe fn take_buffer<S: Slice + ?Sized, B: Buffer<S>>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B>;
    unsafe fn take_array<T: Send + Sync + 'static, const N: usize>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<[T; N]>;
    unsafe fn mut_data<S: Slice + ?Sized, L: ArcSliceMutLayout>(
        start: NonNull<S::Item>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<(usize, Option<Data>)>;
    // unsafe because we must unsure `L: FromLayout<Self>`
    unsafe fn update_layout<S: Slice + ?Sized, L: ArcSliceLayout>(
        start: NonNull<S::Item>,
        length: usize,
        data: Self::Data,
    ) -> L::Data;
}

#[cfg(not(feature = "inlined"))]
pub struct ArcSlice<S: Slice + ?Sized, L: Layout = DefaultLayout> {
    pub(crate) start: NonNull<S::Item>,
    pub(crate) length: usize,
    data: ManuallyDrop<<L as ArcSliceLayout>::Data>,
}

#[cfg(feature = "inlined")]
#[repr(C)]
pub struct ArcSlice<S: Slice + ?Sized, L: Layout = DefaultLayout> {
    #[cfg(target_endian = "big")]
    pub(crate) length: usize,
    data: ManuallyDrop<<L as ArcSliceLayout>::Data>,
    pub(crate) start: NonNull<S::Item>,
    #[cfg(target_endian = "little")]
    pub(crate) length: usize,
}

unsafe impl<S: Slice + ?Sized, L: Layout> Send for ArcSlice<S, L> {}
unsafe impl<S: Slice + ?Sized, L: Layout> Sync for ArcSlice<S, L> {}

impl<S: Slice + ?Sized, L: Layout> ArcSlice<S, L> {
    pub(crate) const fn new_impl(
        start: NonNull<S::Item>,
        length: usize,
        data: <L as ArcSliceLayout>::Data,
    ) -> Self {
        Self {
            start,
            length,
            data: ManuallyDrop::new(data),
        }
    }

    #[inline]
    pub fn new(slice: &S) -> Self
    where
        S::Item: Copy,
    {
        let (start, length) = slice.to_raw_parts();
        if let Some(empty) = ArcSlice::new_empty(start, length) {
            return empty;
        }
        let (arc, start) = Arc::<S, false>::new(slice);
        Self::new_impl(start, slice.len(), L::data_from_arc_slice(arc))
    }

    pub(crate) fn new_array<const N: usize>(array: [S::Item; N]) -> Self {
        if let Some(empty) = Self::new_empty(NonNull::dangling(), N) {
            return empty;
        }
        let (arc, start) = Arc::<S, false>::new_array(array);
        Self::new_impl(start, N, L::data_from_arc_slice(arc))
    }

    pub(crate) fn new_bytes(slice: &S) -> Self {
        assert_checked(is!(S::Item, u8));
        let (arc, start) = unsafe { Arc::<S, false>::new_unchecked(slice.to_slice()) };
        Self::new_impl(start, slice.len(), L::data_from_arc_slice(arc))
    }

    pub(crate) fn new_vec(mut vec: S::Vec) -> Self {
        if vec.capacity() == 0 {
            return Self::new_array([]);
        }
        if !L::ANY_BUFFER {
            return Self::new_bytes(ManuallyDrop::new(vec).as_slice());
        }
        let start = S::vec_start(&mut vec);
        Self::new_impl(start, vec.len(), L::data_from_vec::<S>(vec))
    }

    fn new_empty(start: NonNull<S::Item>, length: usize) -> Option<Self> {
        let data = L::STATIC_DATA.filter(|_| length == 0)?;
        Some(Self::new_impl(start, length, data))
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.length
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub const fn as_ptr(&self) -> *const S::Item {
        self.start.as_ptr()
    }

    #[inline]
    pub fn borrow(&self, range: impl RangeBounds<usize>) -> ArcSliceBorrow<S, L>
    where
        S: Subsliceable,
    {
        let (offset, len) = offset_len(self.deref(), range);
        unsafe { self.borrow_impl(offset, len) }
    }

    #[inline]
    pub fn borrow_from_ref(&self, subset: &S) -> ArcSliceBorrow<S, L>
    where
        S: Subsliceable,
    {
        let (offset, len) =
            offset_len_subslice(self.deref(), subset).unwrap_or_else(|| panic_out_of_range());
        unsafe { self.borrow_impl(offset, len) }
    }

    pub(crate) unsafe fn borrow_impl(&self, offset: usize, len: usize) -> ArcSliceBorrow<S, L>
    where
        S: Subsliceable,
    {
        ArcSliceBorrow {
            slice: unsafe {
                S::from_slice_unchecked(self.to_slice().get_unchecked(offset..offset + len))
            },
            ptr: L::borrowed_data::<S>(&self.data).unwrap_or_else(|| ptr::from_ref(self).cast()),
            _phantom: PhantomData,
        }
    }

    #[inline]
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self
    where
        S: Subsliceable,
    {
        let (offset, len) = offset_len(self.deref(), range);
        unsafe { self.subslice_impl(offset, len) }
    }

    #[inline]
    pub fn subslice_from_ref(&self, subset: &S) -> Self
    where
        S: Subsliceable,
    {
        let (offset, len) =
            offset_len_subslice(self.deref(), subset).unwrap_or_else(|| panic_out_of_range());
        unsafe { self.subslice_impl(offset, len) }
    }

    pub(crate) unsafe fn subslice_impl(&self, offset: usize, len: usize) -> Self
    where
        S: Subsliceable,
    {
        let start = unsafe { self.start.add(offset) };
        if let Some(empty) = Self::new_empty(start, len) {
            return empty;
        }
        let mut clone = self.clone();
        clone.start = start;
        clone.length = len;
        clone
    }

    #[inline]
    pub fn advance(&mut self, offset: usize)
    where
        S: Subsliceable,
    {
        if offset > self.length {
            panic_out_of_range();
        }
        unsafe { self.check_advance(offset) };
        self.start = unsafe { self.start.add(offset) };
        self.length -= offset;
    }

    #[inline]
    pub fn truncate(&mut self, len: usize)
    where
        S: Subsliceable,
    {
        if len < self.length {
            unsafe { self.check_truncate(len) };
            L::truncate::<S>(self.start, self.length, &mut self.data);
            self.length = len;
        }
    }

    #[inline]
    #[must_use = "consider `ArcSlice::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self
    where
        S: Subsliceable,
    {
        if at == 0 {
            return mem::replace(self, unsafe { self.subslice_impl(0, 0) });
        } else if at == self.length {
            return unsafe { self.subslice_impl(at, 0) };
        } else if at > self.length {
            panic_out_of_range();
        }
        let mut clone = self.clone();
        clone.start = unsafe { clone.start.add(at) };
        clone.length -= at;
        self.length = at;
        clone
    }

    #[inline]
    #[must_use = "consider `ArcSlice::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self
    where
        S: Subsliceable,
    {
        if at == 0 {
            return unsafe { self.subslice_impl(0, 0) };
        } else if at == self.length {
            return mem::replace(self, unsafe { self.subslice_impl(self.len(), 0) });
        } else if at > self.length {
            panic_out_of_range();
        }
        let mut clone = self.clone();
        clone.length = at;
        self.start = unsafe { self.start.add(at) };
        self.length -= at;
        clone
    }

    #[inline]
    pub fn try_into_mut<L2: LayoutMut + FromLayout<L>>(self) -> Result<ArcSliceMut<S, L2>, Self> {
        let mut this = ManuallyDrop::new(self);
        match unsafe { L::mut_data::<S, L2>(this.start, this.length, &mut this.data) } {
            Some((capacity, data)) => Ok(ArcSliceMut::new_impl(
                this.start,
                this.length,
                capacity,
                data,
            )),
            None => Err(ManuallyDrop::into_inner(this)),
        }
    }

    #[inline]
    pub fn is_unique(&self) -> bool {
        L::is_unique::<S>(&self.data)
    }

    #[inline]
    pub fn metadata<M: Any>(&self) -> Option<&M> {
        L::get_metadata::<S, M>(&self.data)
    }

    #[inline]
    pub fn try_into_buffer<B: Buffer<S>>(self) -> Result<B, Self> {
        let mut this = ManuallyDrop::new(self);
        unsafe { L::take_buffer::<S, B>(this.start, this.length, &mut this.data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }

    #[inline]
    pub fn with_layout<L2: Layout + FromLayout<L>>(self) -> ArcSlice<S, L2> {
        let mut this = ManuallyDrop::new(self);
        let data = unsafe { ManuallyDrop::take(&mut this.data) };
        ArcSlice {
            start: this.start,
            length: this.length,
            data: ManuallyDrop::new(unsafe {
                L::update_layout::<S, L2>(this.start, this.length, data)
            }),
        }
    }

    #[inline]
    pub fn into_arc_slice(self) -> ArcSlice<[S::Item], L> {
        let mut this = ManuallyDrop::new(self);
        ArcSlice {
            start: this.start,
            length: this.length,
            data: ManuallyDrop::new(unsafe { ManuallyDrop::take(&mut this.data) }),
        }
    }

    #[allow(clippy::type_complexity)]
    #[inline]
    pub fn try_from_arc_slice(
        slice: ArcSlice<[S::Item], L>,
    ) -> Result<Self, (S::TryFromSliceError, ArcSlice<[S::Item], L>)> {
        match S::try_from_slice(&slice) {
            Ok(_) => Ok(unsafe { Self::from_arc_slice_unchecked(slice) }),
            Err(error) => Err((error, slice)),
        }
    }

    #[allow(clippy::missing_safety_doc)]
    #[inline]
    pub unsafe fn from_arc_slice_unchecked(slice: ArcSlice<[S::Item], L>) -> Self {
        unsafe { assume!(S::try_from_slice(&slice).is_ok()) };
        let mut slice = ManuallyDrop::new(slice);
        Self {
            start: slice.start,
            length: slice.length,
            data: ManuallyDrop::new(unsafe { ManuallyDrop::take(&mut slice.data) }),
        }
    }

    #[inline]
    pub fn drop_with_unique_hint(self) {
        let mut this = ManuallyDrop::new(self);
        unsafe { L::drop::<S, true>(this.start, this.length, &mut this.data) };
    }
}

impl<S: Slice + ?Sized, L: AnyBufferLayout> ArcSlice<S, L> {
    pub(crate) fn from_buffer_impl<B: DynBuffer + Buffer<S>>(buffer: B) -> Self {
        let (arc, start, length) = Arc::new_buffer(buffer);
        Self::new_impl(start, length, L::data_from_arc_buffer::<S, true, B>(arc))
    }

    #[cfg(feature = "raw-buffer")]
    fn from_raw_buffer_impl<B: DynBuffer + RawBuffer<S>>(buffer: B) -> Self {
        let ptr = buffer.into_raw();
        if let Some(data) = L::data_from_raw_buffer::<S, B>(ptr) {
            let buffer = ManuallyDrop::new(unsafe { B::from_raw(ptr) });
            let (start, length) = buffer.as_slice().to_raw_parts();
            return Self::new_impl(start, length, data);
        }
        Self::from_buffer_impl(unsafe { B::from_raw(ptr) })
    }

    #[inline]
    pub fn from_buffer<B: Buffer<S>>(buffer: B) -> Self {
        Self::from_buffer_with_metadata(buffer, ())
    }

    #[inline]
    pub fn from_buffer_with_metadata<B: Buffer<S>, M: Send + Sync + 'static>(
        mut buffer: B,
        metadata: M,
    ) -> Self {
        if is!(M, ()) {
            match try_transmute::<B, &'static S>(buffer) {
                Ok(slice) => return Self::from_static(slice),
                Err(b) => buffer = b,
            }
            match try_transmute::<B, Box<S>>(buffer) {
                Ok(boxed) => return Self::from(boxed),
                Err(b) => buffer = b,
            }
            match try_transmute::<B, S::Vec>(buffer) {
                Ok(vec) => return Self::new_vec(vec),
                Err(b) => buffer = b,
            }
        }
        Self::from_buffer_impl(BufferWithMetadata::new(buffer, metadata))
    }

    #[inline]
    pub fn from_buffer_with_borrowed_metadata<B: Buffer<S> + BorrowMetadata>(buffer: B) -> Self {
        Self::from_buffer_impl(buffer)
    }

    #[cfg(feature = "raw-buffer")]
    #[inline]
    pub fn from_raw_buffer<B: RawBuffer<S>>(buffer: B) -> Self {
        Self::from_raw_buffer_impl(BufferWithMetadata::new(buffer, ()))
    }

    #[cfg(feature = "raw-buffer")]
    #[inline]
    pub fn from_raw_buffer_and_borrowed_metadata<B: RawBuffer<S> + BorrowMetadata>(
        buffer: B,
    ) -> Self {
        Self::from_buffer_impl(buffer)
    }

    pub(crate) fn from_static(slice: &'static S) -> Self {
        let (start, length) = slice.to_raw_parts();
        Self::new_impl(start, length, L::data_from_static(slice))
    }
}

#[cfg(feature = "const-slice")]
impl<L: Layout> ArcSlice<[u8], L> {
    #[inline]
    pub const fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.start.as_ptr(), self.len()) }
    }
}

#[cfg(feature = "const-slice")]
impl<L: Layout> ArcSlice<str, L> {
    #[inline]
    pub const fn as_slice(&self) -> &str {
        let start = self.start.as_ptr();
        let len = self.len();
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(start, len)) }
    }
}

impl<L: StaticLayout> ArcSlice<[u8], L> {
    pub const fn new_static(slice: &'static [u8]) -> Self {
        // MSRV 1.65 const `<*const _>::cast_mut` + 1.85 const `NonNull::new`
        let start = unsafe { NonNull::new_unchecked(slice.as_ptr() as _) };
        let length = slice.len();
        let data = unsafe { L::STATIC_DATA_UNCHECKED.assume_init() };
        Self::new_impl(start, length, data)
    }
}

impl<L: StaticLayout> ArcSlice<str, L> {
    pub const fn new_static(slice: &'static str) -> Self {
        // MSRV 1.65 const `<*const _>::cast_mut` + 1.85 const `NonNull::new`
        let start = unsafe { NonNull::new_unchecked(slice.as_ptr() as _) };
        let length = slice.len();
        let data = unsafe { L::STATIC_DATA_UNCHECKED.assume_init() };
        Self::new_impl(start, length, data)
    }
}

impl<S: Slice + ?Sized, L: Layout> Drop for ArcSlice<S, L> {
    #[inline]
    fn drop(&mut self) {
        unsafe { L::drop::<S, false>(self.start, self.length, &mut self.data) };
    }
}

impl<S: Slice + ?Sized, L: Layout> Clone for ArcSlice<S, L> {
    #[inline]
    fn clone(&self) -> Self {
        let data = L::clone::<S>(self.start, self.length, &self.data);
        Self::new_impl(self.start, self.length, data)
    }
}

impl<S: Slice + ?Sized, L: Layout> Deref for ArcSlice<S, L> {
    type Target = S;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { S::from_raw_parts(self.start, self.len()) }
    }
}

impl<S: Slice + ?Sized, L: Layout> AsRef<S> for ArcSlice<S, L> {
    #[inline]
    fn as_ref(&self) -> &S {
        self
    }
}

impl<S: Hash + Slice + ?Sized, L: Layout> Hash for ArcSlice<S, L> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.deref().hash(state);
    }
}

impl<S: Slice + ?Sized, L: Layout> Borrow<S> for ArcSlice<S, L> {
    #[inline]
    fn borrow(&self) -> &S {
        self
    }
}

impl<S: Slice + ?Sized, L: StaticLayout> Default for ArcSlice<S, L>
where
    for<'a> &'a S: Default,
{
    #[inline]
    fn default() -> Self {
        Self::new_empty(NonNull::dangling(), 0).unwrap_checked()
    }
}

impl<S: fmt::Debug + Slice + ?Sized, L: Layout> fmt::Debug for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self.deref(), f)
    }
}

impl<S: fmt::Display + Slice + ?Sized, L: Layout> fmt::Display for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::LowerHex for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self.to_slice(), f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: Layout> fmt::UpperHex for ArcSlice<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self.to_slice(), f)
    }
}

impl<S: PartialEq + Slice + ?Sized, L: Layout> PartialEq for ArcSlice<S, L> {
    fn eq(&self, other: &ArcSlice<S, L>) -> bool {
        self.deref() == other.deref()
    }
}

impl<S: PartialEq + Slice + ?Sized, L: Layout> Eq for ArcSlice<S, L> {}

impl<S: PartialOrd + Slice + ?Sized, L: Layout> PartialOrd for ArcSlice<S, L> {
    fn partial_cmp(&self, other: &ArcSlice<S, L>) -> Option<cmp::Ordering> {
        self.deref().partial_cmp(other.deref())
    }
}

impl<S: Ord + Slice + ?Sized, L: Layout> Ord for ArcSlice<S, L> {
    fn cmp(&self, other: &ArcSlice<S, L>) -> cmp::Ordering {
        self.deref().cmp(other.deref())
    }
}

impl<S: PartialEq + Slice + ?Sized, L: Layout> PartialEq<S> for ArcSlice<S, L> {
    fn eq(&self, other: &S) -> bool {
        self.deref() == other
    }
}

impl<'a, S: PartialEq + Slice + ?Sized, L: Layout> PartialEq<&'a S> for ArcSlice<S, L> {
    fn eq(&self, other: &&'a S) -> bool {
        self.deref() == *other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<[T; N]>
    for ArcSlice<[T], L>
{
    fn eq(&self, other: &[T; N]) -> bool {
        *other == **self
    }
}

impl<'a, T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<&'a [T; N]>
    for ArcSlice<[T], L>
{
    fn eq(&self, other: &&'a [T; N]) -> bool {
        **other == **self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<ArcSlice<[T], L>>
    for [T; N]
{
    fn eq(&self, other: &ArcSlice<[T], L>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<ArcSlice<[T], L>> for [T] {
    fn eq(&self, other: &ArcSlice<[T], L>) -> bool {
        **other == *self
    }
}

impl<L: Layout> PartialEq<ArcSlice<str, L>> for str {
    fn eq(&self, other: &ArcSlice<str, L>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<Vec<T>> for ArcSlice<[T], L> {
    fn eq(&self, other: &Vec<T>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<String> for ArcSlice<str, L> {
    fn eq(&self, other: &String) -> bool {
        **self == **other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<ArcSlice<[T], L>> for Vec<T> {
    fn eq(&self, other: &ArcSlice<[T], L>) -> bool {
        **self == **other
    }
}

impl<L: Layout> PartialEq<ArcSlice<str, L>> for String {
    fn eq(&self, other: &ArcSlice<str, L>) -> bool {
        **self == **other
    }
}

impl<'a, S: Slice + ?Sized, L: Layout> From<&'a S> for ArcSlice<S, L>
where
    S::Item: Copy,
{
    #[inline]
    fn from(value: &'a S) -> Self {
        Self::new(value)
    }
}

impl<S: Slice + ?Sized, L: AnyBufferLayout> From<Box<S>> for ArcSlice<S, L> {
    #[inline]
    fn from(value: Box<S>) -> Self {
        Self::new_vec(unsafe { S::from_vec_unchecked(value.into_boxed_slice().into_vec()) })
    }
}

impl<T: Send + Sync + 'static, L: AnyBufferLayout> From<Vec<T>> for ArcSlice<[T], L> {
    #[inline]
    fn from(value: Vec<T>) -> Self {
        Self::new_vec(value)
    }
}

impl<L: AnyBufferLayout> From<String> for ArcSlice<str, L> {
    #[inline]
    fn from(value: String) -> Self {
        Self::new_vec(value)
    }
}

impl<T: Send + Sync + 'static, L: Layout, const N: usize> From<[T; N]> for ArcSlice<[T], L> {
    #[inline]
    fn from(value: [T; N]) -> Self {
        Self::new_array(value)
    }
}

impl<T: Send + Sync + 'static, L: Layout, const N: usize> TryFrom<ArcSlice<[T], L>> for [T; N] {
    type Error = ArcSlice<[T], L>;
    #[inline]
    fn try_from(value: ArcSlice<[T], L>) -> Result<Self, Self::Error> {
        let mut this = ManuallyDrop::new(value);
        unsafe { L::take_array::<T, N>(this.start, this.length, &mut this.data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }
}

impl<L: Layout> FromStr for ArcSlice<str, L> {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.into())
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;

    impl<L: Layout> std::io::Read for ArcSlice<[u8], L> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = cmp::min(self.len(), buf.len());
            buf[..n].copy_from_slice(&self[..n]);
            Ok(n)
        }
    }
};

#[derive(Clone, Copy)]
pub struct ArcSliceBorrow<'a, S: Slice + ?Sized, L: Layout = DefaultLayout> {
    slice: &'a S,
    ptr: *const (),
    _phantom: PhantomData<&'a ArcSlice<S, L>>,
}

unsafe impl<S: Slice + ?Sized, L: Layout> Send for ArcSliceBorrow<'_, S, L> {}
unsafe impl<S: Slice + ?Sized, L: Layout> Sync for ArcSliceBorrow<'_, S, L> {}

impl<S: Slice + ?Sized, L: Layout> Deref for ArcSliceBorrow<'_, S, L> {
    type Target = S;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.slice
    }
}

impl<S: fmt::Debug + Slice + ?Sized, L: Layout> fmt::Debug for ArcSliceBorrow<'_, S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self.deref(), f)
    }
}

impl<S: Slice + ?Sized, L: Layout> ArcSliceBorrow<'_, S, L> {
    #[inline]
    pub fn to_owned(self) -> ArcSlice<S, L> {
        let (start, length) = self.slice.to_raw_parts();
        if let Some(empty) = ArcSlice::new_empty(start, length) {
            return empty;
        }
        let data = L::clone_borrowed_data::<S>(self.ptr).unwrap_or_else(|| {
            let arc_slice = unsafe { &*self.ptr.cast::<ArcSlice<S, L>>() };
            L::clone::<S>(arc_slice.start, arc_slice.length, &arc_slice.data)
        });
        ArcSlice {
            start,
            length,
            data: ManuallyDrop::new(data),
        }
    }
}
