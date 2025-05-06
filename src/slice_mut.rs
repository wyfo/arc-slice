use alloc::vec::Vec;
use core::{
    any::Any,
    borrow::{Borrow, BorrowMut},
    cmp, fmt,
    hash::{Hash, Hasher},
    mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    slice,
};

#[allow(unused_imports)]
use crate::msrv::{NonNullExt, StrictProvenance};
use crate::{
    arc::{unit_metadata, Arc},
    buffer::{BorrowMetadata, BufferMut, BufferMutExt},
    error::TryReserveError,
    layout::{
        AnyBufferLayout, AnyBufferLayoutMut, Layout, LayoutMut, OptimizedLayout,
        OptimizedLayoutMut, VecLayoutMut,
    },
    macros::is,
    msrv::{ptr, NonZero, SubPtrExt},
    utils::{debug_slice, panic_out_of_range},
    ArcSlice,
};

pub trait ArcSliceMutLayout {}

impl<const ANY_BUFFER: bool, const UNIQUE_HINT: bool> ArcSliceMutLayout
    for OptimizedLayoutMut<ANY_BUFFER, UNIQUE_HINT>
{
}
impl ArcSliceMutLayout for VecLayoutMut {}

pub struct ArcSliceMut<T: Send + Sync + 'static> {
    start: NonNull<T>,
    length: usize,
    capacity: usize,
    arc_or_offset: NonNull<()>,
}

const VEC_FLAG: usize = 0b01;
const VEC_OFFSET_SHIFT: usize = 1;

enum Inner<T> {
    Vec {
        offset: usize,
    },
    Arc {
        arc: ManuallyDrop<Arc<T>>,
        is_tail: bool,
    },
}

impl<T: Send + Sync + 'static> ArcSliceMut<T> {
    const TAIL_FLAG: usize = if mem::needs_drop::<T>() { 0b10 } else { 0 };

    #[inline]
    pub fn new<B: BufferMut<T>>(buffer: B) -> Self {
        Self::with_metadata(buffer, ())
    }

    #[inline]
    pub fn with_metadata<B: BufferMut<T>, M: Send + Sync + 'static>(
        mut buffer: B,
        metadata: M,
    ) -> Self {
        if is!(M, ()) {
            match buffer.try_into_vec() {
                Ok(vec) => return Self::new_vec(vec),
                Err(b) => buffer = b,
            }
        }
        let (arc, start, length, capacity) = Arc::new_mut(buffer, metadata, 1);
        Self::from_arc(start, length, capacity, arc)
    }

    /// # Safety
    ///
    /// Calling [`B::borrow_metadata`](BorrowMetadata::borrow_metadata) must not invalidate
    /// the buffer slice borrow. The returned metadata must not be used to invalidate the
    /// buffer slice.
    #[inline]
    pub unsafe fn with_borrowed_metadata<B: BufferMut<T> + BorrowMetadata>(buffer: B) -> Self {
        let (arc, start, length, capacity) = Arc::new_borrow_mut(buffer);
        Self::from_arc(start, length, capacity, arc)
    }

    fn set_tail_flag(&mut self) {
        if self.length < self.capacity {
            self.arc_or_offset = self
                .arc_or_offset
                .map_addr(|addr| NonZero::new(addr.get() | Self::TAIL_FLAG).unwrap().into());
        }
    }

    fn spare_capacity(&self) -> usize {
        self.capacity - self.length
    }

    fn update_arc_spare_capacity(&self, arc: &Arc<T>, is_tail: bool) {
        if is_tail {
            unsafe { arc.set_spare_capacity(self.spare_capacity()) };
        }
    }

    fn new_vec(vec: Vec<T>) -> Self {
        let mut vec = ManuallyDrop::new(vec);
        let arc_of_offset = ptr::without_provenance_mut::<()>(VEC_FLAG);
        Self {
            start: NonNull::new(vec.as_mut_ptr()).unwrap(),
            length: vec.len(),
            capacity: vec.capacity(),
            arc_or_offset: NonNull::new(arc_of_offset).unwrap(),
        }
    }

    unsafe fn rebuild_vec(&self, offset: usize) -> Vec<T> {
        unsafe {
            Vec::from_raw_parts(
                self.start.as_ptr().sub(offset),
                offset + self.length,
                offset + self.capacity,
            )
        }
    }

    /// # Safety
    ///
    /// `start` and `length` must represent a valid slice for the slice buffer.
    pub(crate) unsafe fn set_start_len(&mut self, start: NonNull<T>, len: usize) {
        self.start = self.start.with_addr(start.addr());
        self.length = len;
    }

    pub(crate) fn from_arc(start: NonNull<T>, length: usize, capacity: usize, arc: Arc<T>) -> Self {
        let mut this = Self {
            start,
            length,
            capacity,
            arc_or_offset: arc.into_ptr(),
        };
        this.set_tail_flag();
        this
    }

    #[inline(always)]
    fn inner(&self) -> Inner<T> {
        let arc_or_offset = self.arc_or_offset.addr().get();
        if arc_or_offset & VEC_FLAG != 0 {
            Inner::Vec {
                offset: arc_or_offset >> VEC_OFFSET_SHIFT,
            }
        } else {
            let masked_ptr = self.arc_or_offset.map_addr(|addr| {
                unsafe { NonZero::new_unchecked(addr.get() & !Self::TAIL_FLAG) }.into()
            });
            Inner::Arc {
                arc: ManuallyDrop::new(unsafe { Arc::from_ptr(masked_ptr) }),
                is_tail: arc_or_offset & Self::TAIL_FLAG != 0,
            }
        }
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

    /// # Safety
    ///
    /// No uninitialized memory shall be written in the returned slice.
    #[inline]
    pub unsafe fn spare_capacity_mut(&mut self) -> &mut [MaybeUninit<T>] {
        unsafe {
            slice::from_raw_parts_mut(
                self.start.as_ptr().add(self.length).cast(),
                self.spare_capacity(),
            )
        }
    }

    /// # Safety
    ///
    /// First `len` items of the slice must be initialized.
    #[inline]
    pub unsafe fn set_len(&mut self, new_len: usize) {
        self.length = new_len;
    }

    fn set_offset(&mut self, offset: usize) {
        let arc_or_offset =
            ptr::without_provenance_mut::<()>(VEC_FLAG | (offset << VEC_OFFSET_SHIFT));
        self.arc_or_offset = NonNull::new(arc_or_offset).unwrap();
    }

    fn remove_tail_flag(&mut self) {
        self.arc_or_offset = self.arc_or_offset.map_addr(|addr| {
            unsafe { NonZero::new_unchecked(addr.get() & !Self::TAIL_FLAG) }.into()
        });
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len >= self.length {
            return;
        }
        if mem::needs_drop::<T>() {
            match self.inner() {
                Inner::Vec { .. } => unsafe {
                    ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                        self.start.as_ptr().add(len),
                        self.length - len,
                    ));
                },
                Inner::Arc { is_tail, .. } => {
                    if is_tail {
                        self.remove_tail_flag();
                    }
                    self.capacity = len;
                }
            }
        }
        self.length = len;
    }

    #[inline]
    pub fn advance(&mut self, offset: usize) {
        if offset > self.length {
            panic_out_of_range();
        }
        if let Inner::Vec { offset: prev_off } = self.inner() {
            self.set_offset(prev_off + offset);
        }
        self.start = unsafe { self.start.add(offset) };
        self.length -= offset;
        self.capacity -= offset;
    }

    #[cold]
    unsafe fn clone_vec(&mut self, offset: usize) -> Self {
        let vec = unsafe { self.rebuild_vec(offset) };
        if vec.capacity() != 0 {
            let (arc, _, _, _) = Arc::new_mut(vec, (), 2);
            self.arc_or_offset = arc.into_ptr();
            self.set_tail_flag();
        }
        unsafe { ptr::read(self) }
    }

    unsafe fn clone(&mut self) -> Self {
        match self.inner() {
            Inner::Vec { offset } => return unsafe { self.clone_vec(offset) },
            Inner::Arc { arc, .. } => {
                let _ = arc.clone();
            }
        };
        unsafe { ptr::read(self) }
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
        self.remove_tail_flag();
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
        clone.remove_tail_flag();
        clone.capacity = at;
        clone.length = at;
        self.start = unsafe { self.start.add(at) };
        self.capacity -= at;
        self.length -= at;
        clone
    }

    #[inline]
    pub fn try_unsplit(&mut self, other: ArcSliceMut<T>) -> Result<(), ArcSliceMut<T>> {
        let end = unsafe { self.start.add(self.length) };
        let mut other_arc_or_offset = other.arc_or_offset.addr().get();
        if mem::needs_drop::<T>() {
            other_arc_or_offset &= !Self::TAIL_FLAG;
        };
        if end == other.start
            && matches!(self.inner(), Inner::Arc { .. })
            && self.arc_or_offset.addr().get() == other_arc_or_offset
        {
            debug_assert_eq!(self.length, self.capacity);
            // assign arc to have tail flag
            self.arc_or_offset = other.arc_or_offset;
            self.length += other.length;
            self.capacity += other.capacity;
            return Ok(());
        }
        Err(other)
    }

    #[inline]
    pub fn freeze<L: Layout>(self) -> ArcSlice<T, L> {
        let this = ManuallyDrop::new(self);
        match this.inner() {
            Inner::Vec { offset, .. } => unsafe {
                ArcSlice::new_vec_with_offset(this.start, this.length, this.capacity, offset)
            },
            Inner::Arc { arc, is_tail } => {
                this.update_arc_spare_capacity(&arc, is_tail);
                unsafe { ArcSlice::from_arc(this.start, this.len(), ManuallyDrop::into_inner(arc)) }
            }
        }
    }

    #[allow(unstable_name_collisions)]
    unsafe fn shift_vec(&self, mut vec: Vec<T>) -> Vec<T> {
        unsafe {
            let offset = self.start.as_ptr().sub_ptr(vec.as_mut_ptr());
            vec.shift_left(offset, self.length)
        };
        vec
    }

    #[inline]
    pub fn into_vec(self) -> Vec<T>
    where
        T: Clone,
    {
        let this = ManuallyDrop::new(self);
        match this.inner() {
            Inner::Vec { offset } => unsafe { this.shift_vec(this.rebuild_vec(offset)) },
            Inner::Arc { mut arc, is_tail } => unsafe {
                this.update_arc_spare_capacity(&arc, is_tail);
                let mut vec = MaybeUninit::<Vec<T>>::uninit();
                if !arc.take_buffer(this.length, NonNull::new(vec.as_mut_ptr()).unwrap()) {
                    let vec = this.as_slice().to_vec();
                    drop(ManuallyDrop::into_inner(arc));
                    return vec;
                }
                this.shift_vec(vec.assume_init())
            },
        }
    }

    #[inline]
    pub fn metadata<M: Any>(&self) -> Option<&M> {
        match self.inner() {
            Inner::Arc { arc, .. } => arc.get_metadata(),
            _ if is!(M, ()) => Some(unit_metadata()),
            _ => None,
        }
    }

    #[inline]
    pub fn downcast_buffer<B: BufferMut<T>>(self) -> Result<B, Self> {
        let mut buffer = MaybeUninit::<B>::uninit();
        match self.inner() {
            Inner::Vec { offset } if is!(B, Vec<T>) => unsafe {
                let vec_ptr = buffer.as_mut_ptr().cast::<Vec<T>>();
                vec_ptr.write(self.shift_vec(self.rebuild_vec(offset)));
            },
            Inner::Arc { mut arc, is_tail } => unsafe {
                self.update_arc_spare_capacity(&arc, is_tail);
                if !arc.take_buffer(self.length, NonNull::from(&mut buffer).cast::<B>()) {
                    return Err(self);
                }
                if is!(B, Vec<T>) {
                    let vec_ptr = buffer.as_mut_ptr().cast::<Vec<T>>();
                    vec_ptr.write(self.shift_vec(vec_ptr.read()));
                }
            },
            _ => return Err(self),
        }
        mem::forget(self);
        Ok(unsafe { buffer.assume_init() })
    }

    #[cold]
    pub fn try_reserve_inner(
        &mut self,
        additional: usize,
        allocate: bool,
    ) -> Result<(), TryReserveError> {
        match self.inner() {
            Inner::Vec { offset } => {
                let mut vec = unsafe { ManuallyDrop::new(self.rebuild_vec(offset)) };
                // `BufferMutExt::try_reclaim_or_reserve` could be used directly,
                // but it would lead to extra work for nothing.
                if unsafe { vec.try_reclaim(offset, self.length, additional) } {
                    self.set_offset(0);
                    self.start = NonNull::new(vec.as_mut_ptr()).unwrap();
                    self.capacity = vec.capacity();
                    return Ok(());
                } else if !allocate {
                    return Err(TryReserveError::Unsupported);
                }
                vec.reserve(additional);
                let new_start = unsafe { vec.as_mut_ptr().add(offset) };
                self.start = NonNull::new(new_start).unwrap();
                self.capacity = vec.capacity() - offset;
            }
            Inner::Arc { mut arc, is_tail } => {
                self.update_arc_spare_capacity(&arc, is_tail);
                let (res, new_start) =
                    unsafe { arc.try_reserve(additional, allocate, self.start, self.length) };
                self.start = new_start;
                match res {
                    Ok(capa) => self.capacity = capa,
                    Err(err) => return Err(err),
                }
            }
        }
        Ok(())
    }

    #[inline]
    pub fn try_reclaim(&mut self, additional: usize) -> bool {
        if additional < self.spare_capacity() {
            return true;
        }
        self.try_reserve_inner(additional, false).is_ok()
    }

    #[inline]
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        if additional <= self.spare_capacity() {
            return Ok(());
        }
        self.try_reserve_inner(additional, true)
    }

    #[inline]
    pub fn try_extend_from_slice(&mut self, slice: &[T]) -> Result<(), TryReserveError> {
        self.try_reserve(slice.len())?;
        unsafe {
            let end = self.spare_capacity_mut().as_mut_ptr().cast();
            ptr::copy_nonoverlapping(slice.as_ptr(), end, slice.len());
            self.set_len(self.length + slice.len());
        }
        Ok(())
    }
}

unsafe impl<T: Send + Sync + 'static> Send for ArcSliceMut<T> {}
unsafe impl<T: Send + Sync + 'static> Sync for ArcSliceMut<T> {}

impl<T: Send + Sync + 'static> Drop for ArcSliceMut<T> {
    #[inline]
    fn drop(&mut self) {
        match self.inner() {
            Inner::Vec { offset } => drop(unsafe { self.rebuild_vec(offset) }),
            Inner::Arc { arc, is_tail } => {
                self.update_arc_spare_capacity(&arc, is_tail);
                drop(ManuallyDrop::into_inner(arc));
            }
        }
    }
}

impl<T: Send + Sync + 'static> Deref for ArcSliceMut<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Send + Sync + 'static> DerefMut for ArcSliceMut<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T: Send + Sync + 'static> AsRef<[T]> for ArcSliceMut<T> {
    #[inline]
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T: Send + Sync + 'static> AsMut<[T]> for ArcSliceMut<T> {
    #[inline]
    fn as_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Hash + Send + Sync + 'static> Hash for ArcSliceMut<T> {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<T: Send + Sync + 'static> Borrow<[T]> for ArcSliceMut<T> {
    #[inline]
    fn borrow(&self) -> &[T] {
        self
    }
}

impl<T: Send + Sync + 'static> BorrowMut<[T]> for ArcSliceMut<T> {
    #[inline]
    fn borrow_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Send + Sync + 'static> Default for ArcSliceMut<T> {
    #[inline]
    fn default() -> Self {
        Self::new_vec(Vec::new())
    }
}

impl<T: fmt::Debug + Send + Sync + 'static> fmt::Debug for ArcSliceMut<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_slice(self, f)
    }
}

impl fmt::LowerHex for ArcSliceMut<u8> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        lower_hex(self, f)
    }
}

impl fmt::UpperHex for ArcSliceMut<u8> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        upper_hex(self, f)
    }
}

impl<T: PartialEq + Send + Sync + 'static> PartialEq for ArcSliceMut<T> {
    fn eq(&self, other: &ArcSliceMut<T>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq + Send + Sync + 'static> Eq for ArcSliceMut<T> {}

impl<T: PartialOrd + Send + Sync + 'static> PartialOrd for ArcSliceMut<T> {
    fn partial_cmp(&self, other: &ArcSliceMut<T>) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Ord + Send + Sync + 'static> Ord for ArcSliceMut<T> {
    fn cmp(&self, other: &ArcSliceMut<T>) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<T: PartialEq + Send + Sync + 'static> PartialEq<[T]> for ArcSliceMut<T> {
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}

impl<T: PartialEq + Send + Sync + 'static> PartialEq<ArcSliceMut<T>> for [T] {
    fn eq(&self, other: &ArcSliceMut<T>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static, const N: usize> PartialEq<[T; N]> for ArcSliceMut<T> {
    fn eq(&self, other: &[T; N]) -> bool {
        self.as_slice() == other
    }
}

impl<T: PartialEq + Send + Sync + 'static, const N: usize> PartialEq<ArcSliceMut<T>> for [T; N] {
    fn eq(&self, other: &ArcSliceMut<T>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static> PartialEq<Vec<T>> for ArcSliceMut<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        *self == other[..]
    }
}

impl<T: PartialEq + Send + Sync + 'static> PartialEq<ArcSliceMut<T>> for Vec<T> {
    fn eq(&self, other: &ArcSliceMut<T>) -> bool {
        *other == *self
    }
}

impl<T: PartialEq + Send + Sync + 'static> PartialEq<ArcSliceMut<T>> for &[T] {
    fn eq(&self, other: &ArcSliceMut<T>) -> bool {
        *other == *self
    }
}

impl<'a, T: PartialEq + Send + Sync + 'static, O: ?Sized> PartialEq<&'a O> for ArcSliceMut<T>
where
    ArcSliceMut<T>: PartialEq<O>,
{
    fn eq(&self, other: &&'a O) -> bool {
        *self == **other
    }
}

impl<T: Send + Sync + 'static> From<Vec<T>> for ArcSliceMut<T> {
    #[inline]
    fn from(value: Vec<T>) -> Self {
        Self::new_vec(value)
    }
}

impl<T: Send + Sync + 'static, const N: usize> From<[T; N]> for ArcSliceMut<T> {
    #[inline]
    fn from(value: [T; N]) -> Self {
        Self::new(value)
    }
}

impl<T: Clone + Send + Sync + 'static> From<ArcSliceMut<T>> for Vec<T> {
    #[inline]
    fn from(value: ArcSliceMut<T>) -> Self {
        value.into_vec()
    }
}

impl fmt::Write for ArcSliceMut<u8> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.spare_capacity() >= s.len() {
            self.try_extend_from_slice(s.as_bytes()).unwrap();
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }

    #[inline]
    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::write(self, args)
    }
}

struct Plop<L: LayoutMut>(L);
impl<L: AnyBufferLayoutMut> Plop<L> {
    fn try_freeze<L2: Layout>(self) -> Result<L2, Self> {
        todo!()
    }
    fn freeze<L2: AnyBufferLayout>(self) -> L2 {
        todo!()
    }
}

impl<const UNIQUE_HINT: bool> Plop<OptimizedLayoutMut<false, UNIQUE_HINT>> {
    fn freeze<const STATIC2: bool, const UNIQUE_HINT2: bool>(
        self,
    ) -> OptimizedLayout<false, STATIC2, UNIQUE_HINT2> {
        todo!()
    }
}
