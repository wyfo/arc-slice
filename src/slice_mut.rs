use alloc::{string::String, vec::Vec};
use core::{
    any::Any,
    borrow::{Borrow, BorrowMut},
    cmp,
    convert::Infallible,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    slice,
};

#[cfg(not(feature = "oom-handling"))]
use crate::layout::{CloneNoAllocLayout, VecLayout};
#[allow(unused_imports)]
use crate::msrv::{NonNullExt, OptionExt, StrictProvenance};
use crate::{
    arc::Arc,
    buffer::{
        BorrowMetadata, BufferExt, BufferMut, BufferWithMetadata, Concatenable, DynBuffer,
        Emptyable, Extendable, Slice, SliceExt, Zeroable,
    },
    error::{AllocError, AllocErrorImpl, TryReserveError},
    layout::{AnyBufferLayout, DefaultLayoutMut, FromLayout, LayoutMut},
    macros::{assume, is},
    msrv::{ptr, NonZero},
    slice::ArcSliceLayout,
    utils::{
        debug_slice, lower_hex, min_non_zero_cap, panic_out_of_range, transmute_checked,
        try_transmute, upper_hex, UnwrapChecked,
    },
    ArcSlice,
};
#[cfg(feature = "serde")]
use crate::{buffer::Buffer, utils::assert_checked};

mod arc;
mod vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Data(NonNull<()>);

impl Data {
    fn addr(&self) -> NonZero<usize> {
        self.0.addr().into()
    }

    fn into_arc<S: Slice + ?Sized, const ANY_BUFFER: bool>(
        self,
    ) -> ManuallyDrop<Arc<S, ANY_BUFFER>> {
        ManuallyDrop::new(unsafe { Arc::from_raw(self.0) })
    }
}

impl From<NonNull<()>> for Data {
    fn from(value: NonNull<()>) -> Self {
        Self(value)
    }
}

impl<S: Slice + ?Sized, const ANY_BUFFER: bool> From<Arc<S, ANY_BUFFER>> for Data {
    fn from(value: Arc<S, ANY_BUFFER>) -> Self {
        Self(value.into_raw())
    }
}

pub(crate) type TryReserveResult<T> = (Result<usize, TryReserveError>, NonNull<T>);

#[allow(clippy::missing_safety_doc)]
pub unsafe trait ArcSliceMutLayout {
    unsafe fn data_from_vec<S: Slice + ?Sized, E: AllocErrorImpl>(
        vec: S::Vec,
        offset: usize,
    ) -> Result<Data, S::Vec>;
    fn clone<S: Slice + ?Sized, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data,
    ) -> Result<(), E>;
    unsafe fn drop<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data,
    );
    fn advance<S: Slice + ?Sized>(_data: Option<&mut Data>, _offset: usize) {}
    fn truncate<S: Slice + ?Sized>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        _data: &mut Data,
    ) {
    }
    fn get_metadata<S: Slice + ?Sized, M: Any>(data: &Data) -> Option<&M>;
    unsafe fn take_buffer<S: Slice + ?Sized, B: BufferMut<S>, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Option<B>;
    unsafe fn take_array<T: Send + Sync + 'static, const N: usize, const UNIQUE: bool>(
        start: NonNull<T>,
        length: usize,
        data: Data,
    ) -> Option<[T; N]>;
    fn is_unique<S: Slice + ?Sized>(data: Data) -> bool;
    fn try_reserve<S: Slice + ?Sized, const UNIQUE: bool>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: &mut Data,
        additional: usize,
        allocate: bool,
    ) -> TryReserveResult<S::Item>;
    fn frozen_data<S: Slice + ?Sized, L: ArcSliceLayout, E: AllocErrorImpl>(
        start: NonNull<S::Item>,
        length: usize,
        capacity: usize,
        data: Data,
    ) -> Result<L::Data, E>;
    // unsafe because we must unsure `L: FromLayout<Self>`
    unsafe fn update_layout<S: Slice + ?Sized, L: ArcSliceMutLayout, E: AllocErrorImpl>(
        _start: NonNull<S::Item>,
        _length: usize,
        _capacity: usize,
        data: Data,
    ) -> Result<Data, E> {
        Ok(data)
    }
}

pub struct ArcSliceMut<
    S: Slice + ?Sized,
    L: LayoutMut = DefaultLayoutMut,
    const UNIQUE: bool = true,
> {
    start: NonNull<S::Item>,
    length: usize,
    capacity: usize,
    data: Option<Data>,
    _phantom: PhantomData<L>,
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> ArcSliceMut<S, L, UNIQUE> {
    pub const fn len(&self) -> usize {
        self.length
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn as_ptr(&self) -> *const S::Item {
        self.start.as_ptr()
    }

    pub fn as_mut_ptr(&mut self) -> *mut S::Item {
        self.start.as_ptr()
    }

    pub fn as_slice(&self) -> &S {
        unsafe { S::from_raw_parts(self.start, self.len()) }
    }

    pub fn as_mut_slice(&mut self) -> &mut S {
        unsafe { S::from_raw_parts_mut(self.start, self.len()) }
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    fn spare_capacity(&self) -> usize {
        self.capacity - self.length
    }

    /// # Safety
    ///
    /// Writing uninitialized memory may be unsound if the underlying buffer doesn't support it.
    pub unsafe fn spare_capacity_mut(&mut self) -> &mut [MaybeUninit<S::Item>]
    where
        S: Extendable,
    {
        unsafe {
            let end = self.start.as_ptr().add(self.length).cast();
            slice::from_raw_parts_mut(end, self.spare_capacity())
        }
    }

    /// # Safety
    ///
    /// First `len` items of the slice must be initialized.
    pub unsafe fn set_len(&mut self, new_len: usize)
    where
        S: Extendable,
    {
        self.length = new_len;
    }

    pub(crate) fn push(&mut self, item: S::Item)
    where
        S: Extendable,
    {
        self.try_reserve(1).unwrap();
        unsafe { self.start.as_ptr().add(self.length).write(item) };
        self.length += 1;
    }

    pub fn try_reclaim(&mut self, additional: usize) -> bool {
        self.try_reserve_impl(additional, false).is_ok()
    }

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
            Some(data) => L::try_reserve::<S, UNIQUE>(
                self.start,
                self.length,
                self.capacity,
                data,
                additional,
                allocate,
            ),
            None if allocate => {
                let capacity = cmp::max(min_non_zero_cap::<S::Item>(), additional);
                let (arc, start) = Arc::<S>::with_capacity::<AllocError, false>(capacity)?;
                self.data = Some(arc.into());
                (Ok(capacity), start)
            }
            None => return Err(TryReserveError::Unsupported),
        };
        self.start = start;
        self.capacity = capacity?;
        Ok(())
    }

    pub fn try_extend_from_slice(&mut self, slice: &S) -> Result<(), TryReserveError>
    where
        S: Concatenable,
        S::Item: Copy,
    {
        self.try_reserve(slice.len())?;
        unsafe { self.extend_from_slice_unchecked(slice.to_slice()) };
        Ok(())
    }

    unsafe fn extend_from_slice_unchecked(&mut self, slice: &[S::Item])
    where
        S: Concatenable,
        S::Item: Copy,
    {
        unsafe {
            let end = self.start.as_ptr().add(self.length);
            ptr::copy_nonoverlapping(slice.as_ptr(), end, slice.len());
            self.length += slice.len();
        }
    }

    pub fn advance(&mut self, offset: usize) {
        if offset > self.length {
            panic_out_of_range();
        }
        L::advance::<S>(self.data.as_mut(), offset);
        self.start = unsafe { self.start.add(offset) };
        self.length -= offset;
        self.capacity -= offset;
    }

    pub fn truncate(&mut self, len: usize) {
        if len >= self.length {
            return;
        }
        if S::needs_drop() {
            let truncate = <L as ArcSliceMutLayout>::truncate::<S>;
            let data = unsafe { self.data.as_mut().unwrap_unchecked() };
            truncate(self.start, self.length, self.capacity, data);
            // shorten capacity to avoid overwriting droppable items
            self.capacity = len;
        }
        self.length = len;
    }

    pub fn metadata<M: Any>(&self) -> Option<&M> {
        <L as ArcSliceMutLayout>::get_metadata::<S, M>(self.data.as_ref()?)
    }

    pub fn try_into_buffer<B: BufferMut<S>>(self) -> Result<B, Self> {
        // MSRV 1.65 let-else
        let data = match self.data {
            Some(data) => data,
            None => return Err(self),
        };
        let this = ManuallyDrop::new(self);
        let take_buffer = <L as ArcSliceMutLayout>::take_buffer::<S, B, UNIQUE>;
        unsafe { take_buffer(this.start, this.length, this.capacity, data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }

    #[inline(always)]
    pub fn into_unique(self) -> Result<ArcSliceMut<S, L, true>, Self> {
        let is_unique = <L as ArcSliceMutLayout>::is_unique::<S>;
        if !UNIQUE && !self.data.is_some_and(is_unique) {
            return Err(self);
        }
        Ok(unsafe { mem::transmute::<Self, ArcSliceMut<S, L, true>>(self) })
    }

    #[inline(always)]
    pub fn into_shared(self) -> ArcSliceMut<S, L, false> {
        unsafe { mem::transmute::<Self, ArcSliceMut<S, L, false>>(self) }
    }

    fn freeze_impl<L2: FromLayout<L>, E: AllocErrorImpl>(self) -> Result<ArcSlice<S, L2>, Self> {
        let this = ManuallyDrop::new(self);
        let data = match this.data {
            Some(data) => L::frozen_data::<S, L2, E>(this.start, this.length, this.capacity, data),
            None => {
                L2::data_from_static::<_, E>(unsafe { S::from_raw_parts(this.start, this.length) })
                    .map_err(E::forget)
            }
        };
        match data {
            Ok(data) => Ok(ArcSlice::init(this.start, this.length, data)),
            Err(_) => Err(ManuallyDrop::into_inner(this)),
        }
    }

    pub fn try_freeze<L2: FromLayout<L>>(self) -> Result<ArcSlice<S, L2>, Self> {
        self.freeze_impl::<L2, AllocError>()
    }

    fn with_layout_impl<L2: LayoutMut + FromLayout<L>, E: AllocErrorImpl>(
        self,
    ) -> Result<ArcSliceMut<S, L2, UNIQUE>, Self> {
        let this = ManuallyDrop::new(self);
        let update_layout = <L as ArcSliceMutLayout>::update_layout::<S, L2, E>;
        Ok(ArcSliceMut {
            start: this.start,
            length: this.length,
            capacity: this.capacity,
            data: this
                .data
                .map(|data| unsafe { update_layout(this.start, this.length, this.capacity, data) })
                .transpose()
                .map_err(|_| ManuallyDrop::into_inner(this))?,
            _phantom: PhantomData,
        })
    }

    pub fn try_with_layout<L2: LayoutMut + FromLayout<L>>(
        self,
    ) -> Result<ArcSliceMut<S, L2, UNIQUE>, Self> {
        self.with_layout_impl::<L2, AllocError>()
    }

    pub fn into_arc_slice_mut(self) -> ArcSliceMut<[S::Item], L, UNIQUE> {
        let this = ManuallyDrop::new(self);
        ArcSliceMut {
            start: this.start,
            length: this.length,
            capacity: this.capacity,
            data: this.data,
            _phantom: PhantomData,
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn try_from_arc_slice_mut(
        slice: ArcSliceMut<[S::Item], L, UNIQUE>,
    ) -> Result<Self, (S::TryFromSliceError, ArcSliceMut<[S::Item], L, UNIQUE>)> {
        match S::try_from_slice(&slice) {
            Ok(_) => Ok(unsafe { Self::from_arc_slice_mut_unchecked(slice) }),
            Err(error) => Err((error, slice)),
        }
    }

    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn from_arc_slice_mut_unchecked(slice: ArcSliceMut<[S::Item], L, UNIQUE>) -> Self {
        unsafe { assume!(S::try_from_slice(&slice).is_ok()) };
        let slice = ManuallyDrop::new(slice);
        Self {
            start: slice.start,
            length: slice.length,
            capacity: slice.capacity,
            data: slice.data,
            _phantom: slice._phantom,
        }
    }
}

impl<
        S: Slice + ?Sized,
        #[cfg(feature = "oom-handling")] L: LayoutMut,
        #[cfg(not(feature = "oom-handling"))] L: LayoutMut + CloneNoAllocLayout,
        const UNIQUE: bool,
    > ArcSliceMut<S, L, UNIQUE>
{
    pub fn freeze<L2: FromLayout<L>>(self) -> ArcSlice<S, L2> {
        self.freeze_impl::<L2, Infallible>().unwrap_checked()
    }

    pub fn with_layout<L2: LayoutMut + FromLayout<L>>(self) -> ArcSliceMut<S, L2, UNIQUE> {
        self.with_layout_impl::<L2, Infallible>().unwrap_checked()
    }
}

impl<S: Slice + ?Sized, L: LayoutMut> ArcSliceMut<S, L> {
    pub(crate) const fn init(
        start: NonNull<S::Item>,
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

    /// # Safety
    ///
    /// Empty slice must be valid (see [`Emptyable`])
    const unsafe fn empty() -> Self {
        Self::init(NonNull::dangling(), 0, 0, None)
    }

    pub const fn new() -> Self
    where
        S: Emptyable,
    {
        unsafe { Self::empty() }
    }

    pub(crate) fn from_slice_impl<E: AllocErrorImpl>(slice: &S) -> Result<Self, E>
    where
        S::Item: Copy,
    {
        if slice.is_empty() {
            return Ok(unsafe { Self::empty() });
        }
        let (arc, start) = Arc::<S, false>::new::<E>(slice)?;
        Ok(Self::init(
            start,
            slice.len(),
            slice.len(),
            Some(arc.into()),
        ))
    }

    #[cfg(feature = "oom-handling")]
    pub fn from_slice(slice: &S) -> Self
    where
        S::Item: Copy,
    {
        Self::from_slice_impl::<Infallible>(slice).unwrap_checked()
    }

    pub fn try_from_slice(slice: &S) -> Result<Self, AllocError>
    where
        S::Item: Copy,
    {
        Self::from_slice_impl::<AllocError>(slice)
    }

    #[cfg(feature = "serde")]
    pub(crate) fn new_bytes(slice: &S) -> Self {
        assert_checked(is!(S::Item, u8));
        let (arc, start) = unsafe {
            Arc::<S, false>::new_unchecked::<Infallible>(slice.to_slice()).unwrap_checked()
        };
        Self::init(start, slice.len(), slice.len(), Some(arc.into()))
    }

    #[cfg(feature = "serde")]
    pub(crate) fn new_byte_vec(vec: S::Vec) -> Self {
        assert_checked(is!(S::Item, u8));
        if !L::ANY_BUFFER {
            return Self::new_bytes(ManuallyDrop::new(vec).as_slice());
        }
        Self::from_vec(vec)
    }

    pub(crate) fn from_vec_impl<E: AllocErrorImpl>(mut vec: S::Vec) -> Result<Self, S::Vec> {
        let capacity = vec.capacity();
        if capacity == 0 {
            return Ok(unsafe { Self::empty() });
        }
        let start = S::vec_start(&mut vec);
        let length = vec.len();
        let data = unsafe { <L as ArcSliceMutLayout>::data_from_vec::<S, E>(vec, 0)? };
        Ok(Self::init(start, length, capacity, Some(data)))
    }

    pub(crate) fn from_vec(vec: S::Vec) -> Self {
        Self::from_vec_impl::<Infallible>(vec).unwrap_checked()
    }

    fn with_capacity_impl<E: AllocErrorImpl, const ZEROED: bool>(
        capacity: usize,
    ) -> Result<Self, E> {
        if capacity == 0 {
            return Ok(unsafe { Self::empty() });
        }
        let (arc, start) = Arc::<S>::with_capacity::<E, ZEROED>(capacity)?;
        let length = if ZEROED { capacity } else { 0 };
        Ok(Self::init(start, length, capacity, Some(arc.into())))
    }

    #[cfg(feature = "oom-handling")]
    pub fn with_capacity(capacity: usize) -> Self
    where
        S: Emptyable,
    {
        Self::with_capacity_impl::<Infallible, false>(capacity).unwrap_checked()
    }

    pub fn try_with_capacity(capacity: usize) -> Result<Self, AllocError>
    where
        S: Emptyable,
    {
        Self::with_capacity_impl::<AllocError, false>(capacity)
    }

    #[cfg(feature = "oom-handling")]
    pub fn zeroed(length: usize) -> Self
    where
        S: Zeroable,
    {
        Self::with_capacity_impl::<Infallible, true>(length).unwrap_checked()
    }

    pub fn try_zeroed(length: usize) -> Result<Self, AllocError>
    where
        S: Zeroable,
    {
        Self::with_capacity_impl::<AllocError, true>(length)
    }

    #[cfg(feature = "oom-handling")]
    pub fn reserve(&mut self, additional: usize) {
        if let Err(err) = self.try_reserve(additional) {
            #[cold]
            fn panic_reserve(err: TryReserveError) -> ! {
                match err {
                    TryReserveError::AllocError => {
                        alloc::alloc::handle_alloc_error(core::alloc::Layout::new::<()>())
                    }
                    err => panic!("{err:?}"),
                }
            }
            panic_reserve(err);
        }
    }

    #[cfg(feature = "oom-handling")]
    pub fn extend_from_slice(&mut self, slice: &S)
    where
        S: Concatenable,
        S::Item: Copy,
    {
        self.reserve(slice.len());
        unsafe { self.extend_from_slice_unchecked(slice.to_slice()) }
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut> ArcSliceMut<[T], L> {
    pub(crate) fn from_array_impl<E: AllocErrorImpl, const N: usize>(
        array: [T; N],
    ) -> Result<Self, [T; N]> {
        if N == 0 {
            return Ok(Self::new());
        }
        let (arc, start) = Arc::<[T], false>::new_array::<E, N>(array)?;
        Ok(Self::init(start, N, N, Some(arc.into())))
    }

    #[cfg(feature = "oom-handling")]
    pub fn from_array<const N: usize>(array: [T; N]) -> Self {
        Self::from_array_impl::<Infallible, N>(array).unwrap_checked()
    }

    pub fn try_from_array<const N: usize>(array: [T; N]) -> Result<Self, [T; N]> {
        Self::from_array_impl::<AllocError, N>(array)
    }
}

impl<S: Slice + ?Sized, L: LayoutMut> ArcSliceMut<S, L, false> {
    unsafe fn clone_impl<E: AllocErrorImpl>(&mut self) -> Result<Self, E> {
        if let Some(data) = &mut self.data {
            <L as ArcSliceMutLayout>::clone::<S, E>(self.start, self.length, self.capacity, data)?;
        }
        Ok(Self {
            start: self.start,
            length: self.length,
            capacity: self.capacity,
            data: self.data,
            _phantom: self._phantom,
        })
    }

    fn split_off_impl<E: AllocErrorImpl>(&mut self, at: usize) -> Result<Self, E> {
        if at > self.capacity {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone_impl()? };
        clone.start = unsafe { clone.start.add(at) };
        clone.capacity -= at;
        self.capacity = at;
        if at > self.length {
            clone.length = 0;
        } else {
            self.length = at;
            clone.length -= at;
        }
        Ok(clone)
    }

    pub fn try_split_off(&mut self, at: usize) -> Result<Self, AllocError> {
        self.split_off_impl::<AllocError>(at)
    }

    fn split_to_impl<E: AllocErrorImpl>(&mut self, at: usize) -> Result<Self, E> {
        if at > self.length {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone_impl()? };
        clone.capacity = at;
        clone.length = at;
        self.start = unsafe { self.start.add(at) };
        self.capacity -= at;
        self.length -= at;
        Ok(clone)
    }

    pub fn try_split_to(&mut self, at: usize) -> Result<Self, AllocError> {
        self.split_to_impl::<AllocError>(at)
    }

    pub fn try_unsplit(
        &mut self,
        other: ArcSliceMut<S, L, false>,
    ) -> Result<(), ArcSliceMut<S, L, false>> {
        let end = unsafe { self.start.add(self.capacity) };
        if self.length == self.capacity && self.data == other.data && end == other.start {
            self.length += other.length;
            self.capacity += other.capacity;
            return Ok(());
        }
        Err(other)
    }
}

impl<
        S: Slice + ?Sized,
        #[cfg(feature = "oom-handling")] L: LayoutMut,
        #[cfg(not(feature = "oom-handling"))] L: LayoutMut + CloneNoAllocLayout,
    > ArcSliceMut<S, L, false>
{
    #[must_use = "consider `ArcSliceMut::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self {
        self.split_off_impl::<Infallible>(at).unwrap_checked()
    }

    #[must_use = "consider `ArcSliceMut::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self {
        self.split_to_impl::<Infallible>(at).unwrap_checked()
    }
}

impl<S: Slice + ?Sized, L: AnyBufferLayout + LayoutMut> ArcSliceMut<S, L> {
    pub(crate) fn from_dyn_buffer_impl<B: DynBuffer + BufferMut<S>, E: AllocErrorImpl>(
        buffer: B,
    ) -> Result<Self, B> {
        let (arc, start, length, capacity) = Arc::new_buffer_mut::<_, E>(buffer)?;
        Ok(Self::init(start, length, capacity, Some(arc.into())))
    }

    fn from_buffer_impl<B: BufferMut<S>, E: AllocErrorImpl>(mut buffer: B) -> Result<Self, B> {
        match try_transmute::<B, S::Vec>(buffer) {
            Ok(vec) => return Self::from_vec_impl::<E>(vec).map_err(transmute_checked),
            Err(b) => buffer = b,
        }
        Self::from_dyn_buffer_impl::<_, E>(BufferWithMetadata::new(buffer, ()))
            .map_err(|b| b.buffer())
    }

    #[cfg(feature = "oom-handling")]
    pub fn from_buffer<B: BufferMut<S>>(buffer: B) -> Self {
        Self::from_buffer_impl::<_, Infallible>(buffer).unwrap_checked()
    }

    #[cfg(feature = "oom-handling")]
    pub fn try_from_buffer<B: BufferMut<S>>(buffer: B) -> Result<Self, B> {
        Self::from_buffer_impl::<_, AllocError>(buffer)
    }

    fn from_buffer_with_metadata_impl<
        B: BufferMut<S>,
        M: Send + Sync + 'static,
        E: AllocErrorImpl,
    >(
        buffer: B,
        metadata: M,
    ) -> Result<Self, (B, M)> {
        if is!(M, ()) {
            return Self::from_buffer_impl::<_, E>(buffer).map_err(|b| (b, metadata));
        }
        Self::from_dyn_buffer_impl::<_, E>(BufferWithMetadata::new(buffer, metadata))
            .map_err(|b| b.into_tuple())
    }

    #[cfg(feature = "oom-handling")]
    pub fn from_buffer_with_metadata<B: BufferMut<S>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Self {
        Self::from_buffer_with_metadata_impl::<_, _, Infallible>(buffer, metadata).unwrap_checked()
    }

    pub fn try_from_buffer_with_metadata<B: BufferMut<S>, M: Send + Sync + 'static>(
        buffer: B,
        metadata: M,
    ) -> Result<Self, (B, M)> {
        Self::from_buffer_with_metadata_impl::<_, _, AllocError>(buffer, metadata)
    }

    #[cfg(feature = "oom-handling")]
    pub fn from_buffer_with_borrowed_metadata<B: BufferMut<S> + BorrowMetadata>(buffer: B) -> Self {
        Self::from_dyn_buffer_impl::<_, Infallible>(buffer).unwrap_checked()
    }

    pub fn try_from_buffer_with_borrowed_metadata<B: BufferMut<S> + BorrowMetadata>(
        buffer: B,
    ) -> Result<Self, B> {
        Self::from_dyn_buffer_impl::<_, AllocError>(buffer)
    }
}

unsafe impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Send
    for ArcSliceMut<S, L, UNIQUE>
{
}
unsafe impl<S: Slice + ?Sized, L: AnyBufferLayout + LayoutMut, const UNIQUE: bool> Sync
    for ArcSliceMut<S, L, UNIQUE>
{
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Drop for ArcSliceMut<S, L, UNIQUE> {
    fn drop(&mut self) {
        if let Some(data) = self.data {
            let drop = <L as ArcSliceMutLayout>::drop::<S, UNIQUE>;
            unsafe { drop(self.start, self.length, self.capacity, data) };
        }
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Deref for ArcSliceMut<S, L, UNIQUE> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> DerefMut for ArcSliceMut<S, L, UNIQUE> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> AsRef<S> for ArcSliceMut<S, L, UNIQUE> {
    fn as_ref(&self) -> &S {
        self
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> AsMut<S> for ArcSliceMut<S, L, UNIQUE> {
    fn as_mut(&mut self) -> &mut S {
        self
    }
}

impl<S: Hash + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Hash
    for ArcSliceMut<S, L, UNIQUE>
{
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Borrow<S> for ArcSliceMut<S, L, UNIQUE> {
    fn borrow(&self) -> &S {
        self
    }
}

impl<S: Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> BorrowMut<S>
    for ArcSliceMut<S, L, UNIQUE>
{
    fn borrow_mut(&mut self) -> &mut S {
        self
    }
}

impl<S: Emptyable + ?Sized, L: LayoutMut> Default for ArcSliceMut<S, L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: fmt::Debug + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::Debug
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self.as_slice(), f)
    }
}

impl<S: fmt::Display + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::Display
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::LowerHex
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self.to_slice(), f)
    }
}

impl<S: Slice<Item = u8> + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::UpperHex
    for ArcSliceMut<S, L, UNIQUE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self.to_slice(), f)
    }
}

impl<S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialEq
    for ArcSliceMut<S, L, UNIQUE>
{
    fn eq(&self, other: &ArcSliceMut<S, L, UNIQUE>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Eq
    for ArcSliceMut<S, L, UNIQUE>
{
}

impl<S: PartialOrd + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialOrd
    for ArcSliceMut<S, L, UNIQUE>
{
    fn partial_cmp(&self, other: &ArcSliceMut<S, L, UNIQUE>) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<S: Ord + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Ord for ArcSliceMut<S, L, UNIQUE> {
    fn cmp(&self, other: &ArcSliceMut<S, L, UNIQUE>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialEq<S>
    for ArcSliceMut<S, L, UNIQUE>
{
    fn eq(&self, other: &S) -> bool {
        self.as_slice() == other
    }
}

impl<'a, S: PartialEq + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> PartialEq<&'a S>
    for ArcSliceMut<S, L, UNIQUE>
{
    fn eq(&self, other: &&'a S) -> bool {
        self.as_slice() == *other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool, const N: usize>
    PartialEq<[T; N]> for ArcSliceMut<[T], L, UNIQUE>
{
    fn eq(&self, other: &[T; N]) -> bool {
        *other == **self
    }
}

impl<
        'a,
        T: PartialEq + Send + Sync + 'static,
        L: LayoutMut,
        const UNIQUE: bool,
        const N: usize,
    > PartialEq<&'a [T; N]> for ArcSliceMut<[T], L, UNIQUE>
{
    fn eq(&self, other: &&'a [T; N]) -> bool {
        **other == **self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool, const N: usize>
    PartialEq<ArcSliceMut<[T], L, UNIQUE>> for [T; N]
{
    fn eq(&self, other: &ArcSliceMut<[T], L, UNIQUE>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool>
    PartialEq<ArcSliceMut<[T], L, UNIQUE>> for [T]
{
    fn eq(&self, other: &ArcSliceMut<[T], L, UNIQUE>) -> bool {
        **other == *self
    }
}

impl<L: LayoutMut, const UNIQUE: bool> PartialEq<ArcSliceMut<str, L, UNIQUE>> for str {
    fn eq(&self, other: &ArcSliceMut<str, L, UNIQUE>) -> bool {
        **other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> PartialEq<Vec<T>>
    for ArcSliceMut<[T], L, UNIQUE>
{
    fn eq(&self, other: &Vec<T>) -> bool {
        **self == **other
    }
}

impl<T: PartialEq + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool>
    PartialEq<ArcSliceMut<[T], L, UNIQUE>> for Vec<T>
{
    fn eq(&self, other: &ArcSliceMut<[T], L, UNIQUE>) -> bool {
        **self == **other
    }
}

impl<L: LayoutMut, const UNIQUE: bool> PartialEq<String> for ArcSliceMut<str, L, UNIQUE> {
    fn eq(&self, other: &String) -> bool {
        **self == **other
    }
}

impl<L: LayoutMut, const UNIQUE: bool> PartialEq<ArcSliceMut<str, L, UNIQUE>> for String {
    fn eq(&self, other: &ArcSliceMut<str, L, UNIQUE>) -> bool {
        **self == **other
    }
}

#[cfg(feature = "oom-handling")]
impl<'a, S: Slice + ?Sized, L: LayoutMut> From<&'a S> for ArcSliceMut<S, L>
where
    S::Item: Copy,
{
    fn from(value: &'a S) -> Self {
        Self::from_slice(value)
    }
}

#[cfg(feature = "oom-handling")]
impl<T: Send + Sync + 'static, L: AnyBufferLayout + LayoutMut> From<Vec<T>>
    for ArcSliceMut<[T], L>
{
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}

#[cfg(not(feature = "oom-handling"))]
impl<T: Send + Sync + 'static> From<Vec<T>> for ArcSliceMut<[T], VecLayout> {
    fn from(value: Vec<T>) -> Self {
        Self::from_vec(value)
    }
}

impl<T: Send + Sync + 'static, L: LayoutMut, const N: usize, const UNIQUE: bool>
    TryFrom<ArcSliceMut<[T], L, UNIQUE>> for [T; N]
{
    type Error = ArcSliceMut<[T], L, UNIQUE>;
    fn try_from(value: ArcSliceMut<[T], L, UNIQUE>) -> Result<Self, Self::Error> {
        let data = match value.data {
            Some(data) => data,
            None if N == 0 => return Ok(try_transmute::<[T; 0], _>([]).unwrap_checked()),
            None => return Err(value),
        };
        let this = ManuallyDrop::new(value);
        let take_array = <L as ArcSliceMutLayout>::take_array::<T, N, UNIQUE>;
        unsafe { take_array(this.start, this.length, data) }
            .ok_or_else(|| ManuallyDrop::into_inner(this))
    }
}

impl<S: Emptyable + Extendable + ?Sized, L: LayoutMut, const UNIQUE: bool> Extend<S::Item>
    for ArcSliceMut<S, L, UNIQUE>
{
    fn extend<I: IntoIterator<Item = S::Item>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.try_reserve(iter.size_hint().0).unwrap();
        for item in iter {
            self.push(item);
        }
    }
}

impl<S: Emptyable + Extendable + ?Sized, L: LayoutMut> FromIterator<S::Item> for ArcSliceMut<S, L> {
    fn from_iter<T: IntoIterator<Item = S::Item>>(iter: T) -> Self {
        let mut this = Self::new();
        let iter = iter.into_iter();
        this.try_reserve(iter.size_hint().0).unwrap();
        let mut len = this.len();
        for item in iter {
            if this.spare_capacity() == 0 {
                this.try_reserve(1).unwrap();
            }
            unsafe { this.start.as_ptr().add(len).write(item) };
            len += 1;
            unsafe { this.set_len(len) }
        }
        this
    }
}

#[cfg(feature = "oom-handling")]
impl<L: LayoutMut> core::str::FromStr for ArcSliceMut<str, L> {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.into())
    }
}

impl<S: Slice<Item = u8> + Extendable + ?Sized, L: LayoutMut, const UNIQUE: bool> fmt::Write
    for ArcSliceMut<S, L, UNIQUE>
{
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.try_reserve(s.len()).map_err(|_| fmt::Error)?;
        unsafe { self.extend_from_slice_unchecked(s.as_bytes()) };
        Ok(())
    }

    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::write(self, args)
    }
}

impl<L: LayoutMut, const UNIQUE: bool> fmt::Write for ArcSliceMut<str, L, UNIQUE> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.try_extend_from_slice(s).map_err(|_| fmt::Error)
    }

    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::write(self, args)
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;

    impl<L: LayoutMut, const UNIQUE: bool> std::io::Read for ArcSliceMut<[u8], L, UNIQUE> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = cmp::min(self.len(), buf.len());
            buf[..n].copy_from_slice(&self[..n]);
            Ok(n)
        }
    }

    impl<L: LayoutMut, const UNIQUE: bool> std::io::Write for ArcSliceMut<[u8], L, UNIQUE> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = cmp::min(self.spare_capacity(), buf.len());
            unsafe { self.extend_from_slice_unchecked(&buf[..n]) };
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
};
