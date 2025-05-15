use alloc::vec::Vec;
use core::{
    any::Any,
    borrow::{Borrow, BorrowMut},
    cmp,
    cmp::max,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    slice,
};

#[allow(unused_imports)]
use crate::msrv::{NonNullExt, OptionExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{BorrowMetadata, BufferMut, BufferWithMetadata, DynBuffer},
    error::TryReserveError,
    layout::{AnyBufferLayout, DefaultLayoutMut, FromLayout, Layout, LayoutMut},
    macros::{assume, is},
    msrv::{ptr, NonZero},
    slice::ArcSliceLayout,
    utils::{
        debug_slice, lower_hex, min_non_zero_cap, panic_out_of_range, try_transmute, upper_hex,
        NewChecked,
    },
    ArcSlice,
};

mod arc;
mod vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Data(NonNull<()>);

impl Data {
    fn addr(&self) -> NonZero<usize> {
        self.0.addr().into()
    }

    fn into_arc<T, const ANY_BUFFER: bool>(self) -> ManuallyDrop<Arc<T, ANY_BUFFER>> {
        ManuallyDrop::new(unsafe { Arc::from_raw(self.0) })
    }
}

impl From<NonNull<()>> for Data {
    fn from(value: NonNull<()>) -> Self {
        Self(value)
    }
}

impl<T, const ANY_BUFFER: bool> From<Arc<T, ANY_BUFFER>> for Data {
    fn from(value: Arc<T, ANY_BUFFER>) -> Self {
        Self(value.into_raw())
    }
}

pub(crate) type TryReserveResult<T> = (Result<usize, TryReserveError>, NonNull<T>);

#[allow(clippy::missing_safety_doc)]
pub unsafe trait ArcSliceMutLayout {
    unsafe fn data_from_vec<T: Send + Sync + 'static>(vec: Vec<T>, offset: usize) -> Data;
    fn clone<T: Send + Sync + 'static>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data,
    );
    unsafe fn drop<T: Send + Sync + 'static, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data,
    );
    fn advance<T>(_data: Option<&mut Data>, _offset: usize) {}
    fn truncate<T: Send + Sync + 'static>(
        _start: NonNull<T>,
        _length: usize,
        _capacity: usize,
        _data: &mut Data,
    ) {
    }
    fn get_metadata<T, M: Any>(data: &Data) -> Option<&M>;
    unsafe fn take_buffer<T: Send + Sync + 'static, B: BufferMut<T>, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Option<B>;
    fn is_unique<T>(data: Data) -> bool;
    fn try_reserve<T: Send + Sync + 'static, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: &mut Data,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<T>;
    fn frozen_data<T: Send + Sync + 'static, L: ArcSliceLayout>(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> L::Data;
    // unsafe because we must unsure `L: FromLayout<Self>`
    unsafe fn update_layout<T: Send + Sync + 'static, L: ArcSliceMutLayout>(
        _start: NonNull<T>,
        _length: usize,
        _capacity: usize,
        data: Data,
    ) -> Data {
        data
    }
}

pub struct ArcSliceMut<
    T: Send + Sync + 'static,
    L: LayoutMut = DefaultLayoutMut,
    const UNIQUE: bool = true,
> {
    start: NonNull<T>,
    length: usize,
    capacity: usize,
    data: Option<Data>,
    _phantom: PhantomData<L>,
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> ArcSliceMut<T, L, UNIQUE> {
    pub fn as_slice(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.start.as_ptr(), self.length) }
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { slice::from_raw_parts_mut(self.start.as_ptr(), self.length) }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn spare_capacity(&self) -> usize {
        self.capacity - self.length
    }

    /// # Safety
    ///
    /// Writing uninitialized memory may be unsound if the underlying buffer doesn't support it.
    #[inline]
    pub unsafe fn spare_capacity_mut(&mut self) -> &mut [MaybeUninit<T>] {
        unsafe {
            let end = self.start.as_ptr().add(self.length).cast();
            slice::from_raw_parts_mut(end, self.spare_capacity())
        }
    }

    /// # Safety
    ///
    /// First `len` items of the slice must be initialized.
    #[inline]
    pub unsafe fn set_len(&mut self, new_len: usize) {
        self.length = new_len;
    }

    #[inline]
    pub fn try_reclaim(&mut self, additional: usize) -> bool {
        self.try_reserve_impl(additional, false).is_ok()
    }

    #[inline]
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.try_reserve_impl(additional, true)
    }

    fn try_reserve_impl(
        &mut self,
        additional: usize,
        allocate: bool,
    ) -> Result<(), TryReserveError> {
        if additional <= self.spare_capacity() {
            return Ok(());
        }
        let res = self.try_reserve_cold(additional, allocate);
        unsafe { assume!(res.is_err() || self.spare_capacity() >= additional) };
        res
    }

    #[cold]
    fn try_reserve_cold(
        &mut self,
        additional: usize,
        allocate: bool,
    ) -> Result<(), TryReserveError> {
        let (capacity, start) = match &mut self.data {
            Some(data) => L::try_reserve::<T, UNIQUE>(
                self.start,
                self.length,
                self.capacity,
                data,
                additional,
                allocate,
            ),
            None if allocate => {
                let capacity = max(min_non_zero_cap::<T>(), additional);
                let (arc, start) = Arc::<T>::with_capacity::<false>(capacity);
                self.data = Some(arc.into());
                (Ok(capacity), start)
            }
            None => return Err(TryReserveError::Unsupported),
        };
        self.start = start;
        self.capacity = capacity?;
        Ok(())
    }

    #[inline]
    pub fn try_extend_from_slice(&mut self, slice: &[T]) -> Result<(), TryReserveError>
    where
        T: Copy,
    {
        self.try_reserve(slice.len())?;
        unsafe { self.extend_from_slice_unchecked(slice) };
        Ok(())
    }

    unsafe fn extend_from_slice_unchecked(&mut self, slice: &[T])
    where
        T: Copy,
    {
        unsafe {
            let end = self.spare_capacity_mut().as_mut_ptr().cast();
            ptr::copy_nonoverlapping(slice.as_ptr(), end, slice.len());
            self.set_len(self.length + slice.len());
        }
    }

    #[inline]
    pub fn advance(&mut self, offset: usize) {
        if offset > self.length {
            panic_out_of_range();
        }
        L::advance::<T>(self.data.as_mut(), offset);
        self.start = unsafe { self.start.add(offset) };
        self.length -= offset;
        self.capacity -= offset;
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len >= self.length {
            return;
        }
        if mem::needs_drop::<T>() {
            let truncate = <L as ArcSliceMutLayout>::truncate;
            let data = unsafe { self.data.as_mut().unwrap_unchecked() };
            truncate(self.start, self.length, self.capacity, data);
            // shorten capacity to avoid overwriting droppable items
            self.capacity = len;
        }
        self.length = len;
    }

    #[inline]
    pub fn metadata<M: Any>(&self) -> Option<&M> {
        <L as ArcSliceMutLayout>::get_metadata::<T, M>(self.data.as_ref()?)
    }

    #[inline]
    pub fn try_into_buffer<B: BufferMut<T>>(self) -> Result<B, Self> {
        // MSRV 1.65 let-else
        let data = match self.data {
            Some(data) => data,
            None => return Err(self),
        };
        let this = ManuallyDrop::new(self);
        let take_buffer = <L as ArcSliceMutLayout>::take_buffer::<T, B, UNIQUE>;
        unsafe { take_buffer(this.start, this.length, this.capacity, data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }

    #[inline(always)]
    pub fn into_unique(self) -> Result<ArcSliceMut<T, L, true>, Self> {
        if UNIQUE {
            return Ok(try_transmute(self).ok().unwrap());
        }
        let is_unique = <L as ArcSliceMutLayout>::is_unique::<T>;
        if !self.data.is_some_and(is_unique) {
            return Err(self);
        }
        Ok(unsafe { mem::transmute::<Self, ArcSliceMut<T, L, true>>(self) })
    }

    #[inline(always)]
    pub fn into_shared(self) -> ArcSliceMut<T, L, false> {
        unsafe { mem::transmute::<Self, ArcSliceMut<T, L, false>>(self) }
    }

    #[inline]
    pub fn freeze<L2: Layout + FromLayout<L>>(self) -> ArcSlice<T, L2> {
        let this = ManuallyDrop::new(self);
        let data = match this.data {
            Some(data) => L::frozen_data::<T, L2>(this.start, this.length, this.capacity, data),
            None => L2::data_from_static(unsafe {
                slice::from_raw_parts(this.start.as_ptr(), this.length)
            }),
        };
        ArcSlice::new_impl(this.start, this.length, data)
    }

    #[inline]
    pub fn with_layout<L2: LayoutMut + FromLayout<L>>(self) -> ArcSliceMut<T, L2, UNIQUE> {
        let this = ManuallyDrop::new(self);
        let update_layout = <L as ArcSliceMutLayout>::update_layout::<T, L2>;
        ArcSliceMut {
            start: this.start,
            length: this.length,
            capacity: this.capacity,
            data: this
                .data
                .map(|data| unsafe { update_layout(this.start, this.length, this.capacity, data) }),
            _phantom: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut> ArcSliceMut<T, L> {
    pub(crate) fn new_impl(
        start: NonNull<T>,
        length: usize,
        capacity: usize,
        data: Option<Data>,
    ) -> Self {
        Self {
            start,
            length,
            capacity,
            data,
            _phantom: PhantomData,
        }
    }

    #[inline]
    pub fn new() -> Self {
        Self::new_impl(NonNull::dangling(), 0, 0, None)
    }

    pub(crate) fn new_slice(slice: &[T]) -> Self
    where
        T: Copy,
    {
        if slice.is_empty() {
            return Self::new();
        }
        let (arc, start) = Arc::<T>::new(slice);
        Self::new_impl(start, slice.len(), slice.len(), Some(arc.into()))
    }

    pub(crate) fn new_array<const N: usize>(array: [T; N]) -> Self {
        if N == 0 {
            return Self::new();
        }
        let (arc, start) = Arc::<T>::new_array(array);
        Self::new_impl(start, N, N, Some(arc.into()))
    }

    fn with_capacity_impl<const ZEROED: bool>(capacity: usize) -> Self {
        if capacity == 0 {
            return Self::new();
        }
        let (arc, start) = Arc::<T>::with_capacity::<ZEROED>(capacity);
        Self::new_impl(start, 0, capacity, Some(arc.into()))
    }

    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_impl::<false>(capacity)
    }

    #[inline]
    pub fn zeroed(capacity: usize) -> Self {
        Self::with_capacity_impl::<true>(capacity)
    }

    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        if let Err(err) = self.try_reserve(additional) {
            #[cold]
            fn panic_reserve(err: TryReserveError) -> ! {
                panic!("{err:?}")
            }
            panic_reserve(err);
        }
    }

    #[inline]
    pub fn extend_from_slice(&mut self, slice: &[T])
    where
        T: Copy,
    {
        self.reserve(slice.len());
        unsafe { self.extend_from_slice_unchecked(slice) }
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut> ArcSliceMut<T, L, false> {
    unsafe fn clone(&mut self) -> Self {
        if let Some(data) = &mut self.data {
            <L as ArcSliceMutLayout>::clone(self.start, self.length, self.capacity, data);
        }
        Self {
            start: self.start,
            length: self.length,
            capacity: self.capacity,
            data: self.data,
            _phantom: self._phantom,
        }
    }

    #[inline]
    #[must_use = "consider `ArcSliceMut::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self {
        if at > self.capacity {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone() };
        clone.start = unsafe { clone.start.add(at) };
        clone.capacity -= at;
        self.capacity = at;
        if at > self.length {
            clone.length = 0;
        } else {
            self.length = at;
            clone.length -= at;
        }
        clone
    }

    #[inline]
    #[must_use = "consider `ArcSliceMut::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self {
        if at > self.length {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone() };
        clone.capacity = at;
        clone.length = at;
        self.start = unsafe { self.start.add(at) };
        self.capacity -= at;
        self.length -= at;
        clone
    }

    #[inline]
    pub fn try_unsplit(
        &mut self,
        other: ArcSliceMut<T, L, false>,
    ) -> Result<(), ArcSliceMut<T, L, false>> {
        let end = unsafe { self.start.add(self.capacity) };
        if self.length == self.capacity && self.data == other.data && end == other.start {
            self.length += other.length;
            self.capacity += other.capacity;
            return Ok(());
        }
        Err(other)
    }
}

impl<T: Send + Sync + 'static, L: AnyBufferLayout + LayoutMut> ArcSliceMut<T, L> {
    pub(crate) fn from_buffer_impl<B: DynBuffer + BufferMut<T>>(buffer: B) -> Self {
        let (arc, start, length, capacity) = Arc::new_buffer_mut(buffer);
        Self::new_impl(start, length, capacity, Some(arc.into()))
    }

    pub fn from_buffer<B: BufferMut<T>>(buffer: B) -> Self {
        Self::from_buffer_with_metadata(buffer, ())
    }

    pub fn from_buffer_with_metadata<B: BufferMut<T>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Self {
        if is!(M, ()) {
            return buffer.into_arc_slice_mut();
        }
        Self::from_buffer_impl(BufferWithMetadata::new(buffer, metadata))
    }

    pub fn from_buffer_with_borrowed_metadata<B: BufferMut<T> + BorrowMetadata>(buffer: B) -> Self {
        Self::from_buffer_impl(buffer)
    }

    pub(crate) fn from_vec(mut vec: Vec<T>) -> Self {
        let capacity = vec.capacity();
        if capacity == 0 {
            return Self::new();
        }
        let start = NonNull::new_checked(vec.as_mut_ptr());
        let length = vec.len();
        let data = unsafe { <L as ArcSliceMutLayout>::data_from_vec(vec, 0) };
        Self::new_impl(start, length, capacity, Some(data))
    }
}

unsafe impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Send
    for ArcSliceMut<T, L, UNIQUE>
{
}
unsafe impl<T: Send + Sync + 'static, L: AnyBufferLayout + LayoutMut, const UNIQUE: bool> Sync
    for ArcSliceMut<T, L, UNIQUE>
{
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Drop
    for ArcSliceMut<T, L, UNIQUE>
{
    #[inline]
    fn drop(&mut self) {
        if let Some(data) = self.data {
            let drop = <L as ArcSliceMutLayout>::drop::<T, UNIQUE>;
            unsafe { drop(self.start, self.length, self.capacity, data) };
        }
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Deref
    for ArcSliceMut<T, L, UNIQUE>
{
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> DerefMut
    for ArcSliceMut<T, L, UNIQUE>
{
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> AsRef<[T]>
    for ArcSliceMut<T, L, UNIQUE>
{
    #[inline]
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> AsMut<[T]>
    for ArcSliceMut<T, L, UNIQUE>
{
    #[inline]
    fn as_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Hash + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Hash
    for ArcSliceMut<T, L, UNIQUE>
{
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Borrow<[T]>
    for ArcSliceMut<T, L, UNIQUE>
{
    #[inline]
    fn borrow(&self) -> &[T] {
        self
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> BorrowMut<[T]>
    for ArcSliceMut<T, L, UNIQUE>
{
    #[inline]
    fn borrow_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut> Default for ArcSliceMut<T, L> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> fmt::Debug
    for ArcSliceMut<T, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self, f)
    }
}

impl<L: LayoutMut, const UNIQUE: bool> fmt::LowerHex for ArcSliceMut<u8, L, UNIQUE> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self, f)
    }
}

impl<L: LayoutMut, const UNIQUE: bool> fmt::UpperHex for ArcSliceMut<u8, L, UNIQUE> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self, f)
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> PartialEq
    for ArcSliceMut<T, L, UNIQUE>
{
    fn eq(&self, other: &ArcSliceMut<T, L, UNIQUE>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Eq
    for ArcSliceMut<T, L, UNIQUE>
{
}

impl<T: PartialOrd + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> PartialOrd
    for ArcSliceMut<T, L, UNIQUE>
{
    fn partial_cmp(&self, other: &ArcSliceMut<T, L, UNIQUE>) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Ord + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Ord
    for ArcSliceMut<T, L, UNIQUE>
{
    fn cmp(&self, other: &ArcSliceMut<T, L, UNIQUE>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> PartialEq<[T]>
    for ArcSliceMut<T, L, UNIQUE>
{
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool>
    PartialEq<ArcSliceMut<T, L, UNIQUE>> for [T]
{
    fn eq(&self, other: &ArcSliceMut<T, L, UNIQUE>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool, const N: usize>
    PartialEq<[T; N]> for ArcSliceMut<T, L, UNIQUE>
{
    fn eq(&self, other: &[T; N]) -> bool {
        self.as_slice() == other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool, const N: usize>
    PartialEq<ArcSliceMut<T, L, UNIQUE>> for [T; N]
{
    fn eq(&self, other: &ArcSliceMut<T, L, UNIQUE>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> PartialEq<Vec<T>>
    for ArcSliceMut<T, L, UNIQUE>
{
    fn eq(&self, other: &Vec<T>) -> bool {
        *self == other[..]
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool>
    PartialEq<ArcSliceMut<T, L, UNIQUE>> for Vec<T>
{
    fn eq(&self, other: &ArcSliceMut<T, L, UNIQUE>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool>
    PartialEq<ArcSliceMut<T, L, UNIQUE>> for &[T]
{
    fn eq(&self, other: &ArcSliceMut<T, L, UNIQUE>) -> bool {
        *other == *self
    }
}

impl<'a, T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool, O: ?Sized>
    PartialEq<&'a O> for ArcSliceMut<T, L, UNIQUE>
where
    ArcSliceMut<T, L, UNIQUE>: PartialEq<O>,
{
    fn eq(&self, other: &&'a O) -> bool {
        *self == **other
    }
}

impl<'a, T: Copy + Send + Sync + 'static, L: AnyBufferLayout + LayoutMut> From<&'a [T]>
    for ArcSliceMut<T, L>
{
    #[inline]
    fn from(value: &'a [T]) -> Self {
        Self::new_slice(value)
    }
}

impl<T: Send + Sync + 'static, L: AnyBufferLayout + LayoutMut> From<Vec<T>> for ArcSliceMut<T, L> {
    #[inline]
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const N: usize> From<[T; N]> for ArcSliceMut<T, L> {
    #[inline]
    fn from(value: [T; N]) -> Self {
        Self::new_array(value)
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Extend<T>
    for ArcSliceMut<T, L, UNIQUE>
{
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.try_reserve(iter.size_hint().0).unwrap();
        let mut len = self.len();
        for item in iter {
            if self.spare_capacity() == 0 {
                self.try_reserve(1).unwrap();
            }
            unsafe { self.start.as_ptr().add(len).write(item) };
            len += 1;
            unsafe { self.set_len(len) }
        }
    }
}

impl<'a, T: Clone + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Extend<&'a T>
    for ArcSliceMut<T, L, UNIQUE>
{
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, iter: I) {
        self.extend(iter.into_iter().cloned());
    }
}

impl<L: LayoutMut, const UNIQUE: bool> fmt::Write for ArcSliceMut<u8, L, UNIQUE> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.try_extend_from_slice(s.as_bytes())
            .map_err(|_| fmt::Error)
    }

    #[inline]
    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::write(self, args)
    }
}
