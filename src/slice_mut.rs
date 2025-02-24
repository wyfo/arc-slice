use alloc::vec::Vec;
use core::{
    any::Any,
    borrow::{Borrow, BorrowMut},
    cmp, fmt, mem,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr,
    ptr::NonNull,
    slice,
};

use crate::{
    arc::{unit_metadata, Arc},
    buffer::{reclaim, BufferMut, TryReserveError},
    layout::Layout,
    macros::is,
    rust_compat::{
        non_null_add, non_null_addr, non_null_map_addr, non_null_with_addr, without_provenance_mut,
    },
    utils::{debug_slice, panic_out_of_range, shrink_vec},
    ArcSlice,
};

pub struct ArcSliceMut<T: Send + Sync + 'static> {
    start: NonNull<T>,
    length: usize,
    capacity: usize,
    arc_or_offset: NonNull<()>,
}

const VEC_FLAG: usize = 0b01;
const VEC_CAPA_SHIFT: usize = 1;

enum Inner {
    Vec {
        offset: usize,
    },
    Arc {
        arc: ManuallyDrop<Arc>,
        is_tail: bool,
    },
}

impl<T: Send + Sync + 'static> ArcSliceMut<T> {
    const TAIL_FLAG: usize = if mem::needs_drop::<T>() { 0b10 } else { 0 };

    pub fn new<B: BufferMut<T>>(buffer: B) -> Self {
        Self::new_with_metadata(buffer, ())
    }

    pub fn new_with_metadata<B: BufferMut<T>, M: Send + Sync + 'static>(
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
        Self {
            start,
            length,
            capacity,
            arc_or_offset: arc.into_ptr(),
        }
        .with_tail_flag()
    }

    fn set_tail_flag(&mut self) {
        if self.length < self.capacity {
            self.arc_or_offset = non_null_map_addr(self.arc_or_offset, |addr| {
                (addr.get() | Self::TAIL_FLAG).try_into().unwrap()
            });
        }
    }

    fn with_tail_flag(mut self) -> Self {
        self.set_tail_flag();
        self
    }

    fn spare_capacity(&self) -> usize {
        self.capacity - self.length
    }

    fn update_arc_spare_capacity(&self, arc: &Arc, is_tail: bool) {
        if is_tail {
            arc.set_spare_capacity(self.spare_capacity());
        }
    }

    fn new_vec(vec: Vec<T>) -> Self {
        let mut vec = ManuallyDrop::new(vec);
        let arc_of_offset = without_provenance_mut::<()>(VEC_FLAG);
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
        self.start = non_null_with_addr(self.start, non_null_addr(start));
        self.length = len;
    }

    pub(crate) fn from_arc<B: BufferMut<T>>(buffer: &mut B, arc: Arc) -> Self {
        // convert the arc before executing `BufferMut` method in case of panic,
        // so the `Arc` will not be dropped
        let arc_or_offset = arc.into_ptr();
        Self {
            start: buffer.as_mut_ptr(),
            length: buffer.len(),
            capacity: buffer.capacity(),
            arc_or_offset,
        }
        .with_tail_flag()
    }

    #[inline(always)]
    fn inner(&self) -> Inner {
        let arc_or_offset = non_null_addr(self.arc_or_offset).get();
        if arc_or_offset & VEC_FLAG != 0 {
            Inner::Vec {
                offset: arc_or_offset >> VEC_CAPA_SHIFT,
            }
        } else {
            let masked_ptr = non_null_map_addr(self.arc_or_offset, |addr| unsafe {
                (addr.get() & !Self::TAIL_FLAG)
                    .try_into()
                    .unwrap_unchecked()
            });
            Inner::Arc {
                arc: ManuallyDrop::new(unsafe { Arc::from_ptr(masked_ptr) }),
                is_tail: arc_or_offset & Self::TAIL_FLAG != 0,
            }
        }
    }

    pub const fn len(&self) -> usize {
        self.length
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn as_slice(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.start.as_ptr(), self.length) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { slice::from_raw_parts_mut(self.start.as_ptr(), self.length) }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// # Safety
    ///
    /// No uninitialized memory shall be written in the returned slice.
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
    pub unsafe fn set_len(&mut self, new_len: usize) {
        self.length = new_len;
    }

    fn set_offset(&mut self, offset: usize) {
        let arc_or_offset = without_provenance_mut::<()>(VEC_FLAG | (offset << VEC_CAPA_SHIFT));
        self.arc_or_offset = NonNull::new(arc_or_offset).unwrap();
    }

    fn remove_tail_flag(&mut self) {
        self.arc_or_offset = non_null_map_addr(self.arc_or_offset, |addr| unsafe {
            (addr.get() & !Self::TAIL_FLAG)
                .try_into()
                .unwrap_unchecked()
        });
    }

    pub fn truncate(&mut self, len: usize) {
        if len >= self.length {
            return;
        }
        if mem::needs_drop::<T>() {
            match self.inner() {
                Inner::Vec { offset } => unsafe {
                    ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                        self.start.as_ptr().add(offset + len),
                        self.length - offset + len,
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

    pub fn advance(&mut self, offset: usize) {
        if offset > self.length {
            panic_out_of_range();
        }
        if let Inner::Vec { offset: prev_off } = self.inner() {
            self.set_offset(prev_off + offset);
        }
        self.start = unsafe { non_null_add(self.start, offset) };
        self.length -= offset;
        self.capacity -= offset;
    }

    #[cold]
    unsafe fn clone_vec(&mut self, offset: usize) -> Self {
        let vec = unsafe { self.rebuild_vec(offset) };
        let (arc, _, _, _) = Arc::new_mut(vec, (), 2);
        self.arc_or_offset = arc.into_ptr();
        self.set_tail_flag();
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

    #[must_use = "consider `ArcSliceMut::truncate` if you don't need the other half"]
    pub fn split_off(&mut self, at: usize) -> Self {
        if at > self.capacity {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone() };
        clone.start = unsafe { non_null_add(clone.start, at) };
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

    #[must_use = "consider `ArcSliceMut::advance` if you don't need the other half"]
    pub fn split_to(&mut self, at: usize) -> Self {
        if at > self.length {
            panic_out_of_range();
        }
        let mut clone = unsafe { self.clone() };
        clone.remove_tail_flag();
        clone.capacity = at;
        clone.length = at;
        self.start = unsafe { non_null_add(self.start, at) };
        self.capacity -= at;
        self.length -= at;
        clone
    }

    pub fn try_unsplit(&mut self, other: ArcSliceMut<T>) -> Result<(), ArcSliceMut<T>> {
        let end = unsafe { non_null_add(self.start, self.length) };
        let mut other_arc_or_offset = non_null_addr(other.arc_or_offset).get();
        if mem::needs_drop::<T>() {
            other_arc_or_offset &= !Self::TAIL_FLAG;
        };
        if end == other.start
            && matches!(self.inner(), Inner::Arc { .. })
            && non_null_addr(self.arc_or_offset).get() == other_arc_or_offset
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

    pub fn freeze<L: Layout>(self) -> ArcSlice<T, L> {
        let this = ManuallyDrop::new(self);
        match this.inner() {
            Inner::Vec { offset, .. } => {
                let mut slice = ArcSlice::new(unsafe { this.rebuild_vec(offset) });
                slice.advance(offset);
                slice
            }
            Inner::Arc { arc, is_tail } => {
                this.update_arc_spare_capacity(&arc, is_tail);
                unsafe { ArcSlice::from_arc(ManuallyDrop::into_inner(arc), this.start, this.len()) }
            }
        }
    }

    unsafe fn shrink_vec(&self, vec: Vec<T>) -> Vec<T> {
        unsafe { shrink_vec(vec, self.start, self.length) }
    }

    pub fn into_vec(self) -> Vec<T>
    where
        T: Clone,
    {
        let this = ManuallyDrop::new(self);
        match this.inner() {
            Inner::Vec { offset } => unsafe { this.shrink_vec(this.rebuild_vec(offset)) },
            Inner::Arc { mut arc, is_tail } => unsafe {
                this.update_arc_spare_capacity(&arc, is_tail);
                let mut vec = MaybeUninit::<Vec<T>>::uninit();
                if !arc.take_buffer(this.length, NonNull::new(vec.as_mut_ptr()).unwrap()) {
                    let vec = this.as_slice().to_vec();
                    drop(ManuallyDrop::into_inner(arc));
                    return vec;
                }
                this.shrink_vec(vec.assume_init())
            },
        }
    }

    pub fn get_metadata<M: Any>(&self) -> Option<&M> {
        match self.inner() {
            Inner::Arc { arc, .. } => arc.get_metadata(),
            _ if is!(M, ()) => Some(unit_metadata()),
            _ => None,
        }
    }

    pub fn downcast_buffer<B: BufferMut<T>>(self) -> Result<B, Self> {
        let mut buffer = MaybeUninit::<B>::uninit();
        match self.inner() {
            Inner::Vec { offset } if is!(B, Vec<T>) => unsafe {
                let vec_ptr = buffer.as_mut_ptr().cast::<Vec<T>>();
                vec_ptr.write(self.shrink_vec(self.rebuild_vec(offset)));
            },
            Inner::Arc { mut arc, is_tail } => unsafe {
                self.update_arc_spare_capacity(&arc, is_tail);
                if !arc.take_buffer(self.length, NonNull::from(&mut buffer).cast::<B>()) {
                    return Err(self);
                }
                if is!(B, Vec<T>) {
                    let vec_ptr = buffer.as_mut_ptr().cast::<Vec<T>>();
                    vec_ptr.write(self.shrink_vec(vec_ptr.read()));
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
                if unsafe { reclaim(&mut *vec, offset, self.length, additional) } {
                    self.set_offset(0);
                    self.start = NonNull::new(vec.as_mut_ptr()).unwrap();
                    self.capacity = vec.capacity();
                    return Ok(());
                } else if !allocate {
                    return Err(TryReserveError::AllocError);
                }
                BufferMut::try_reserve(&mut *vec, additional)?;
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

    pub fn try_reclaim(&mut self, additional: usize) -> bool {
        if additional < self.spare_capacity() {
            return true;
        }
        self.try_reserve_inner(additional, false).is_ok()
    }

    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        if additional <= self.spare_capacity() {
            return Ok(());
        }
        self.try_reserve_inner(additional, true)
    }

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

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Send + Sync + 'static> DerefMut for ArcSliceMut<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T: Send + Sync + 'static> AsRef<[T]> for ArcSliceMut<T> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T: Send + Sync + 'static> AsMut<[T]> for ArcSliceMut<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Send + Sync + 'static> Borrow<[T]> for ArcSliceMut<T> {
    fn borrow(&self) -> &[T] {
        self
    }
}

impl<T: Send + Sync + 'static> BorrowMut<[T]> for ArcSliceMut<T> {
    fn borrow_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Send + Sync + 'static> Default for ArcSliceMut<T> {
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
        for &b in self.as_slice() {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl fmt::UpperHex for ArcSliceMut<u8> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.as_slice() {
            write!(f, "{:02X}", b)?;
        }
        Ok(())
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
    fn from(value: Vec<T>) -> Self {
        Self::new(value)
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
