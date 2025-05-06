use alloc::{boxed::Box, vec::Vec};
use core::{
    any::Any,
    borrow::Borrow,
    cmp, fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, RangeBounds},
    ptr::NonNull,
};

#[cfg(feature = "raw-buffer")]
use crate::buffer::RawBuffer;
#[allow(unused_imports)]
use crate::msrv::{ptr, NonNullExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{BorrowMetadata, Buffer, BufferWithMetadata, DynBuffer},
    layout::{AnyBufferLayout, DefaultLayout, Layout, StaticLayout},
    macros::is,
    slice_mut::ArcSliceMut,
    utils::{
        debug_slice, lower_hex, offset_len, offset_len_subslice, panic_out_of_range,
        slice_into_raw_parts, upper_hex,
    },
};

mod optimized;
// mod raw;
mod vec;

pub(crate) trait ArcSliceLayout: 'static {
    type Data;
    const STATIC_DATA: Option<Self::Data> = None;
    // MSRV 1.83 const `Option::unwrap`
    const STATIC_DATA_UNCHECKED: MaybeUninit<Self::Data> = MaybeUninit::uninit();
    fn data_from_arc<T>(arc: Arc<T>) -> Self::Data;
    fn data_from_static<T: Send + Sync + 'static>(slice: &'static [T]) -> Self::Data {
        Self::STATIC_DATA.unwrap_or_else(|| {
            Self::data_from_arc(Arc::new_buffer(BufferWithMetadata::new(slice, ())).0)
        })
    }
    fn data_from_vec<T: Send + Sync + 'static>(vec: Vec<T>) -> Self::Data;
    fn data_from_raw_buffer<T, B: DynBuffer + Buffer<T>>(_buffer: *const ()) -> Option<Self::Data> {
        None
    }
    fn clone<T: Send + Sync + 'static>(
        start: NonNull<T>,
        length: usize,
        data: &Self::Data,
    ) -> Self::Data;
    unsafe fn drop<T>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
        unique_hint: bool,
    );
    fn borrowed_data<T>(_data: &Self::Data) -> Option<*const ()> {
        None
    }
    fn clone_borrowed_data<T>(_ptr: *const ()) -> Option<Self::Data> {
        None
    }
    fn truncate<T: Send + Sync + 'static>(
        _start: NonNull<T>,
        _length: usize,
        _data: &mut Self::Data,
    ) {
    }
    fn is_unique<T>(data: &Self::Data) -> bool;
    fn get_metadata<T, M: Any>(data: &Self::Data) -> Option<&M>;
    unsafe fn take_buffer<T: Send + Sync + 'static, B: Buffer<T>>(
        start: NonNull<T>,
        length: usize,
        data: &mut ManuallyDrop<Self::Data>,
    ) -> Option<B>;
    // TODO unsafe because we must unsure `L: FromLayout<Self>`
    unsafe fn update_layout<T: Send + Sync + 'static, L: ArcSliceLayout>(
        start: NonNull<T>,
        length: usize,
        data: Self::Data,
    ) -> L::Data;
}

#[cfg(not(feature = "inlined"))]
pub struct ArcSlice<T: Send + Sync + 'static, L: Layout = DefaultLayout> {
    start: NonNull<T>,
    length: usize,
    data: ManuallyDrop<L::Data>,
}

#[cfg(feature = "inlined")]
#[repr(C)]
pub struct ArcSlice<T: Send + Sync + 'static, L: Layout = DefaultLayout> {
    #[cfg(target_endian = "big")]
    length: usize,
    data: <L as ArcSliceLayout>::Data,
    start: NonNull<T>,
    #[cfg(target_endian = "little")]
    length: usize,
}

unsafe impl<T: Send + Sync + 'static, L: Layout> Send for ArcSlice<T, L> {}
unsafe impl<T: Send + Sync + 'static, L: Layout> Sync for ArcSlice<T, L> {}

impl<T: Send + Sync + 'static, L: Layout> ArcSlice<T, L> {
    fn new_impl(start: NonNull<T>, length: usize, data: L::Data) -> Self {
        Self {
            start,
            length,
            data: ManuallyDrop::new(data),
        }
    }

    #[inline]
    pub fn new(slice: &[T]) -> Self
    where
        T: Copy,
    {
        let (arc, start) = Arc::<T>::new(slice);
        Self::new_impl(start, slice.len(), L::data_from_arc(arc))
    }

    fn new_array<const N: usize>(array: [T; N]) -> Self {
        let (arc, start) = Arc::<T>::new_array(array);
        Self::new_impl(start, N, L::data_from_arc(arc))
    }

    fn new_empty(start: NonNull<T>, length: usize) -> Option<Self> {
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
    pub const fn as_slice(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.start.as_ptr(), self.len()) }
    }

    #[inline]
    pub fn borrow(&self, range: impl RangeBounds<usize>) -> ArcSliceBorrow<T, L> {
        let (offset, len) = offset_len(self.length, range);
        unsafe { self.borrow_impl(offset, len) }
    }

    #[inline]
    pub fn borrow_from_ref(&self, subset: &[T]) -> ArcSliceBorrow<T, L> {
        let (offset, len) =
            offset_len_subslice(self, subset).unwrap_or_else(|| panic_out_of_range());
        unsafe { self.borrow_impl(offset, len) }
    }

    pub(crate) unsafe fn borrow_impl(&self, offset: usize, len: usize) -> ArcSliceBorrow<T, L> {
        ArcSliceBorrow {
            slice: unsafe { self.get_unchecked(offset..offset + len) },
            ptr: L::borrowed_data::<T>(&self.data).unwrap_or_else(|| ptr::from_ref(self).cast()),
            _phantom: PhantomData,
        }
    }

    #[inline]
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self {
        let (offset, len) = offset_len(self.length, range);
        unsafe { self.subslice_impl(offset, len) }
    }

    #[inline]
    pub fn subslice_from_ref(&self, subset: &[T]) -> Self {
        let (offset, len) =
            offset_len_subslice(self, subset).unwrap_or_else(|| panic_out_of_range());
        unsafe { self.subslice_impl(offset, len) }
    }

    #[allow(clippy::incompatible_msrv)]
    pub(crate) unsafe fn subslice_impl(&self, offset: usize, len: usize) -> Self {
        // MSRV if-let-chains
        if let Some(empty) = Self::new_empty(unsafe { self.start.add(offset) }, len) {
            return empty;
        }
        let mut clone = self.clone();
        clone.start = unsafe { self.start.add(offset) };
        clone.length = len;
        clone
    }

    #[allow(clippy::incompatible_msrv)]
    #[inline]
    pub fn advance(&mut self, offset: usize) {
        if offset > self.length {
            panic_out_of_range();
        }
        self.start = unsafe { self.start.add(offset) };
        self.length -= offset;
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len < self.length {
            L::truncate(self.start, self.length, &mut self.data);
            self.length = len;
        }
    }

    #[allow(clippy::incompatible_msrv)]
    #[inline]
    #[must_use = "consider `ArcSlice::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self {
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

    #[allow(clippy::incompatible_msrv)]
    #[inline]
    #[must_use = "consider `ArcSlice::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self {
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
    pub fn try_into_mut(self) -> Result<ArcSliceMut<T>, Self> {
        todo!()
    }

    #[inline]
    pub fn is_unique(&self) -> bool {
        L::is_unique::<T>(&self.data)
    }

    #[inline]
    pub fn metadata<M: Any>(&self) -> Option<&M> {
        L::get_metadata::<T, M>(&self.data)
    }

    #[inline]
    pub fn try_into_buffer<B: Buffer<T>>(self) -> Result<B, Self> {
        let mut this = ManuallyDrop::new(self);
        unsafe { L::take_buffer::<T, B>(this.start, this.length, &mut this.data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }

    #[inline]
    pub fn with_layout<L2: Layout>(self) -> ArcSlice<T, L2> {
        let mut this = ManuallyDrop::new(self);
        let data = unsafe { ManuallyDrop::take(&mut this.data) };
        ArcSlice {
            start: this.start,
            length: this.length,
            data: ManuallyDrop::new(unsafe {
                L::update_layout::<T, L2>(this.start, this.length, data)
            }),
        }
    }

    pub fn drop_with_unique_hint(self) {
        let mut this = ManuallyDrop::new(self);
        unsafe { L::drop(this.start, this.length, &mut this.data, true) };
    }
}

impl<T: Send + Sync + 'static, L: StaticLayout> ArcSlice<T, L> {
    pub const fn new_static(slice: &'static [T]) -> Self {
        let (start, length) = slice_into_raw_parts(slice);
        Self {
            start,
            length,
            data: ManuallyDrop::new(unsafe { L::STATIC_DATA_UNCHECKED.assume_init() }),
        }
    }
}

impl<T: Send + Sync + 'static, L: AnyBufferLayout> ArcSlice<T, L> {
    pub(crate) fn from_buffer_impl<B: DynBuffer + Buffer<T>>(buffer: B) -> Self {
        let (arc, start, length) = Arc::new_buffer(buffer);
        Self {
            start,
            length,
            data: ManuallyDrop::new(L::data_from_arc(arc)),
        }
    }

    #[cfg(feature = "raw-buffer")]
    fn from_raw_buffer_impl<B: DynBuffer + RawBuffer<T>>(buffer: B) -> Self {
        let ptr = buffer.into_raw();
        if let Some(data) = L::data_from_raw_buffer::<T, B>(ptr) {
            let buffer = ManuallyDrop::new(unsafe { B::from_raw(ptr) });
            let (start, length) = slice_into_raw_parts(buffer.as_slice());
            return Self {
                start,
                length,
                data: ManuallyDrop::new(data),
            };
        }
        Self::from_buffer_impl(unsafe { B::from_raw(ptr) })
    }

    #[inline]
    pub fn from_buffer<B: Buffer<T>>(buffer: B) -> Self {
        Self::from_buffer_with_metadata(buffer, ())
    }

    #[inline]
    pub fn from_buffer_with_metadata<B: Buffer<T>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Self {
        if is!(M, ()) {
            return B::into_arc_slice(buffer);
        }
        Self::from_buffer_impl(BufferWithMetadata::new(buffer, metadata))
    }

    #[inline]
    pub fn from_buffer_with_borrowed_metadata<B: Buffer<T> + BorrowMetadata>(buffer: B) -> Self {
        Self::from_buffer_impl(buffer)
    }

    #[cfg(feature = "raw-buffer")]
    #[inline]
    pub fn from_raw_buffer<B: RawBuffer<T>>(buffer: B) -> Self {
        Self::from_raw_buffer_impl(BufferWithMetadata::new(buffer, ()))
    }

    #[cfg(feature = "raw-buffer")]
    #[inline]
    pub fn from_raw_buffer_and_borrowed_metadata<B: RawBuffer<T> + BorrowMetadata>(
        buffer: B,
    ) -> Self {
        Self::from_buffer_impl(buffer)
    }

    pub(crate) fn from_static(slice: &'static [T]) -> Self {
        match L::STATIC_DATA {
            Some(data) => Self {
                start: NonNull::new(slice.as_ptr().cast_mut()).unwrap(),
                length: slice.len(),
                data: ManuallyDrop::new(data),
            },
            None => Self::from_buffer_impl(BufferWithMetadata::new(slice, ())),
        }
    }

    pub(crate) fn from_vec(mut vec: Vec<T>) -> Self {
        Self {
            start: NonNull::new(vec.as_mut_ptr()).unwrap(),
            length: vec.len(),
            data: ManuallyDrop::new(L::data_from_vec(vec)),
        }
    }
}

impl<T: Send + Sync + 'static, L: Layout> Drop for ArcSlice<T, L> {
    #[inline]
    fn drop(&mut self) {
        unsafe { L::drop(self.start, self.length, &mut self.data, false) };
    }
}

impl<T: Send + Sync + 'static, L: Layout> Clone for ArcSlice<T, L> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            start: self.start,
            length: self.length,
            data: ManuallyDrop::new(L::clone(self.start, self.length, &self.data)),
        }
    }
}

impl<T: Send + Sync + 'static, L: Layout> Deref for ArcSlice<T, L> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Send + Sync + 'static, L: Layout> AsRef<[T]> for ArcSlice<T, L> {
    #[inline]
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T: Hash + Send + Sync + 'static, L: Layout> Hash for ArcSlice<T, L> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<T: Send + Sync + 'static, L: Layout> Borrow<[T]> for ArcSlice<T, L> {
    #[inline]
    fn borrow(&self) -> &[T] {
        self
    }
}

impl<T: Send + Sync + 'static, L: StaticLayout> Default for ArcSlice<T, L> {
    #[inline]
    fn default() -> Self {
        Self::new_static(&[])
    }
}

impl<T: fmt::Debug + Send + Sync + 'static, L: Layout> fmt::Debug for ArcSlice<T, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self, f)
    }
}

impl<L: Layout> fmt::LowerHex for ArcSlice<u8, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self, f)
    }
}

impl<L: Layout> fmt::UpperHex for ArcSlice<u8, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self, f)
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq for ArcSlice<T, L> {
    fn eq(&self, other: &ArcSlice<T, L>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> Eq for ArcSlice<T, L> {}

impl<T: PartialOrd + Send + Sync + 'static, L: Layout> PartialOrd for ArcSlice<T, L> {
    fn partial_cmp(&self, other: &ArcSlice<T, L>) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Ord + Send + Sync + 'static, L: Layout> Ord for ArcSlice<T, L> {
    fn cmp(&self, other: &ArcSlice<T, L>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<[T]> for ArcSlice<T, L> {
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<ArcSlice<T, L>> for [T] {
    fn eq(&self, other: &ArcSlice<T, L>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<[T; N]>
    for ArcSlice<T, L>
{
    fn eq(&self, other: &[T; N]) -> bool {
        self.as_slice() == other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout, const N: usize> PartialEq<ArcSlice<T, L>>
    for [T; N]
{
    fn eq(&self, other: &ArcSlice<T, L>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<Vec<T>> for ArcSlice<T, L> {
    fn eq(&self, other: &Vec<T>) -> bool {
        *self == other[..]
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<ArcSlice<T, L>> for Vec<T> {
    fn eq(&self, other: &ArcSlice<T, L>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: Layout> PartialEq<ArcSlice<T, L>> for &[T] {
    fn eq(&self, other: &ArcSlice<T, L>) -> bool {
        *other == *self
    }
}

impl<'a, T: PartialEq + Send + Sync + 'static, L: Layout, O: ?Sized> PartialEq<&'a O>
    for ArcSlice<T, L>
where
    ArcSlice<T, L>: PartialEq<O>,
{
    fn eq(&self, other: &&'a O) -> bool {
        *self == **other
    }
}

impl<T: Send + Sync + 'static, L: AnyBufferLayout> From<Box<[T]>> for ArcSlice<T, L> {
    fn from(value: Box<[T]>) -> Self {
        Self::from_vec(value.into())
    }
}

impl<T: Send + Sync + 'static, L: AnyBufferLayout> From<Vec<T>> for ArcSlice<T, L> {
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}

impl<T: Send + Sync + 'static, L: Layout, const N: usize> From<[T; N]> for ArcSlice<T, L> {
    #[inline]
    fn from(value: [T; N]) -> Self {
        Self::new_array(value)
    }
}

#[derive(Clone, Copy)]
pub struct ArcSliceBorrow<'a, T: Send + Sync + 'static, L: Layout = DefaultLayout> {
    slice: &'a [T],
    ptr: *const (),
    _phantom: PhantomData<&'a ArcSlice<T, L>>,
}

unsafe impl<T: Send + Sync + 'static, L: Layout> Send for ArcSliceBorrow<'_, T, L> {}
unsafe impl<T: Send + Sync + 'static, L: Layout> Sync for ArcSliceBorrow<'_, T, L> {}

impl<T: Send + Sync + 'static, L: Layout> Deref for ArcSliceBorrow<'_, T, L> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.slice
    }
}

impl<T: fmt::Debug + Send + Sync + 'static, L: Layout> fmt::Debug for ArcSliceBorrow<'_, T, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: Send + Sync + 'static, L: Layout> ArcSliceBorrow<'_, T, L> {
    #[inline]
    pub fn to_owned(self) -> ArcSlice<T, L> {
        // MSRV if-let-chains
        let (start, length) = slice_into_raw_parts(self.slice);
        if let Some(empty) = ArcSlice::new_empty(start, length) {
            return empty;
        }
        let data = L::clone_borrowed_data::<T>(self.ptr).unwrap_or_else(|| {
            let arc_slice = unsafe { &*self.ptr.cast::<ArcSlice<T, L>>() };
            L::clone(arc_slice.start, arc_slice.length, &arc_slice.data)
        });
        ArcSlice {
            start,
            length,
            data: ManuallyDrop::new(data),
        }
    }
}
