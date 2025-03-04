use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use core::{
    any::Any,
    borrow::Borrow,
    cmp, fmt,
    hash::{Hash, Hasher},
    hint, mem,
    mem::{ManuallyDrop, MaybeUninit},
    num::NonZeroUsize,
    ops::{Deref, RangeBounds},
    ptr,
    ptr::NonNull,
};

use crate::{
    arc::{unit_metadata, Arc},
    buffer::{Buffer, BufferMutExt},
    layout::{Compact, Layout, Plain},
    loom::{
        atomic_ptr_with_mut,
        sync::atomic::{AtomicPtr, Ordering},
    },
    macros::is,
    rust_compat::{non_null_add, non_null_sub_ptr, ptr_addr, sub_ptr, without_provenance_mut},
    utils::{
        debug_slice, offset_len, offset_len_subslice, offset_len_subslice_unchecked,
        panic_out_of_range,
    },
    ArcSliceMut,
};

pub trait ArcSliceLayout {
    type Base: Copy + 'static;
    fn get_base<T>(vec: &mut Vec<T>) -> Option<Self::Base>;
    fn base_into_ptr<T>(base: Self::Base) -> Option<NonNull<T>>;
}

impl ArcSliceLayout for Compact {
    type Base = ();
    fn get_base<T>(vec: &mut Vec<T>) -> Option<Self::Base> {
        (vec.capacity() == vec.len()).then_some(())
    }
    fn base_into_ptr<T>(_base: Self::Base) -> Option<NonNull<T>> {
        None
    }
}

impl ArcSliceLayout for Plain {
    type Base = NonNull<()>;
    fn get_base<T>(vec: &mut Vec<T>) -> Option<Self::Base> {
        Some(NonNull::new(vec.as_mut_ptr()).unwrap().cast())
    }
    fn base_into_ptr<T>(base: Self::Base) -> Option<NonNull<T>> {
        Some(base.cast())
    }
}

#[repr(C)]
pub struct ArcSlice<T: Send + Sync + 'static, L: Layout = Compact> {
    #[cfg(target_endian = "big")]
    length: usize,
    arc_or_capa: AtomicPtr<()>,
    base: MaybeUninit<<L as ArcSliceLayout>::Base>,
    start: NonNull<T>,
    #[cfg(target_endian = "little")]
    length: usize,
}

const VEC_FLAG: usize = 1;
const VEC_CAPA_SHIFT: usize = 1;

enum Inner<T> {
    Static,
    Vec { capacity: NonZeroUsize },
    Arc(ManuallyDrop<Arc<T>>),
}

impl<T: Send + Sync + 'static, L: Layout> ArcSlice<T, L> {
    #[inline]
    pub fn new<B: Buffer<T>>(buffer: B) -> Self {
        Self::with_metadata(buffer, ())
    }

    #[cfg(not(all(loom, test)))]
    #[inline]
    pub const fn new_static(slice: &'static [T]) -> Self {
        Self {
            arc_or_capa: AtomicPtr::new(ptr::null_mut()),
            base: MaybeUninit::uninit(),
            start: unsafe { NonNull::new_unchecked(slice.as_ptr().cast_mut()) },
            length: slice.len(),
        }
    }

    #[cfg(all(loom, test))]
    pub fn new_static(slice: &'static [T]) -> Self {
        Self {
            arc_or_capa: AtomicPtr::new(ptr::null_mut()),
            base: MaybeUninit::uninit(),
            start: NonNull::new(slice.as_ptr().cast_mut()).unwrap(),
            length: slice.len(),
        }
    }

    #[inline]
    pub fn with_metadata<B: Buffer<T>, M: Send + Sync + 'static>(
        mut buffer: B,
        metadata: M,
    ) -> Self {
        if is!(M, ()) {
            match buffer.try_into_static() {
                Ok(slice) => return Self::new_static(slice),
                Err(b) => buffer = b,
            }
            match buffer.try_into_vec() {
                Ok(vec) => return Self::new_vec(vec),
                Err(b) => buffer = b,
            }
        }
        let (arc, start, length) = Arc::new(buffer, metadata, 1);
        unsafe { Self::from_arc(arc, start, length) }
    }

    fn new_vec(mut vec: Vec<T>) -> Self {
        if vec.capacity() == 0 {
            return Self::new_static(&[]);
        }
        let Some(base) = L::get_base(&mut vec) else {
            #[cold]
            fn alloc<T: Send + Sync + 'static, L: Layout>(vec: Vec<T>) -> ArcSlice<T, L> {
                let (arc, start, length) = Arc::new(vec, (), 1);
                unsafe { ArcSlice::from_arc(arc, start, length) }
            }
            return alloc(vec);
        };
        let mut vec = ManuallyDrop::new(vec);
        let arc_or_capa = without_provenance_mut::<()>(VEC_FLAG | (vec.capacity() << 1));
        Self {
            arc_or_capa: AtomicPtr::new(arc_or_capa),
            base: MaybeUninit::new(base),
            start: NonNull::new(vec.as_mut_ptr()).unwrap(),
            length: vec.len(),
        }
    }

    /// # Safety
    ///
    /// `start` and `length` must represent a valid slice for the buffer contained in `arc`.
    pub(crate) unsafe fn from_arc(arc: Arc<T>, start: NonNull<T>, length: usize) -> Self {
        Self {
            arc_or_capa: AtomicPtr::new(arc.into_ptr().as_ptr()),
            base: MaybeUninit::uninit(),
            start,
            length,
        }
    }

    #[inline]
    pub fn from_slice(slice: &[T]) -> Self
    where
        T: Clone,
    {
        if slice.is_empty() {
            Self::new_static(&[])
        } else {
            Self::new_vec(slice.to_vec())
        }
    }

    unsafe fn rebuild_vec(&self, capacity: NonZeroUsize) -> Vec<T> {
        let (ptr, len) = match L::base_into_ptr(unsafe { self.base.assume_init() }) {
            Some(base) => {
                let len = unsafe { non_null_sub_ptr(self.start, base) } + self.length;
                (base.as_ptr(), len)
            }
            None => {
                let ptr = unsafe {
                    self.start
                        .as_ptr()
                        .offset(self.length as isize - capacity.get() as isize)
                };
                (ptr, capacity.get())
            }
        };
        unsafe { Vec::from_raw_parts(ptr, len, capacity.get()) }
    }

    unsafe fn shift_vec(&self, mut vec: Vec<T>) -> Vec<T> {
        unsafe {
            let offset = sub_ptr(self.start.as_ptr(), vec.as_mut_ptr());
            vec.shift_left(offset, self.length)
        };
        vec
    }

    #[inline(always)]
    fn inner(&self, arc_or_capa: *mut ()) -> Inner<T> {
        match NonNull::new(arc_or_capa) {
            Some(_) if ptr_addr(arc_or_capa) & VEC_FLAG != 0 => Inner::Vec {
                capacity: unsafe {
                    NonZeroUsize::new(ptr_addr(arc_or_capa) >> VEC_CAPA_SHIFT).unwrap_unchecked()
                },
            },
            Some(arc) => Inner::Arc(ManuallyDrop::new(unsafe { Arc::from_ptr(arc) })),
            None => Inner::Static,
        }
    }

    #[inline(always)]
    fn inner_mut(&mut self) -> Inner<T> {
        let arc_or_capa = atomic_ptr_with_mut(&mut self.arc_or_capa, |ptr| *ptr);
        self.inner(arc_or_capa)
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
    pub fn get_ref(&self, range: impl RangeBounds<usize>) -> ArcSliceRef<T, L> {
        let (offset, len) = offset_len(self.length, range);
        ArcSliceRef {
            slice: &self[offset..offset + len],
            arc_slice: self,
        }
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len >= self.length {
            return;
        }
        match self.inner_mut() {
            Inner::Vec { .. } if is!(L::Base, ()) => return unsafe { self.truncate_vec(len) },
            Inner::Vec { .. } if mem::needs_drop::<T>() => unsafe {
                let end = self.start.as_ptr().add(len);
                ptr::drop_in_place(ptr::slice_from_raw_parts_mut(end, self.len() - len));
            },
            _ => {}
        }
        self.length = len;
    }

    #[cold]
    unsafe fn truncate_vec(&mut self, len: usize) {
        let Inner::Vec { capacity } = self.inner_mut() else {
            unsafe { hint::unreachable_unchecked() }
        };
        let vec = unsafe { self.rebuild_vec(capacity) };
        let (arc, _, _) = Arc::new(vec, (), 1);
        atomic_ptr_with_mut(&mut self.arc_or_capa, |ptr| {
            *ptr = arc.into_ptr().as_ptr();
        });
        self.length = len;
    }

    #[inline]
    pub fn advance(&mut self, offset: usize) {
        if offset > self.length {
            panic_out_of_range();
        }
        self.start = unsafe { non_null_add(self.start, offset) };
        self.length -= offset;
    }

    pub(crate) unsafe fn subslice_impl(&self, offset: usize, len: usize) -> Self {
        if len == 0 {
            let mut arc_or_capa = self.arc_or_capa.load(Ordering::Acquire);
            match self.inner(arc_or_capa) {
                Inner::Static => {}
                Inner::Vec { .. } => arc_or_capa = ptr::null_mut(),
                Inner::Arc(arc) if arc.get_metadata::<()>().is_some() => {
                    arc_or_capa = ptr::null_mut();
                }
                Inner::Arc(arc) => {
                    let _ = arc.clone();
                }
            };
            return Self {
                arc_or_capa: AtomicPtr::new(arc_or_capa),
                base: MaybeUninit::uninit(),
                start: unsafe { non_null_add(self.start, offset) },
                length: 0,
            };
        }
        let mut clone = self.clone();
        clone.start = unsafe { non_null_add(self.start, offset) };
        clone.length = len;
        clone
    }

    #[inline]
    pub fn subslice(&self, range: impl RangeBounds<usize>) -> Self {
        let (offset, len) = offset_len(self.length, range);
        unsafe { self.subslice_impl(offset, len) }
    }

    #[inline]
    pub fn subslice_from_ref(&self, subset: &[T]) -> Self {
        let (offset, len) = offset_len_subslice(self, subset);
        unsafe { self.subslice_impl(offset, len) }
    }

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
        clone.start = unsafe { non_null_add(clone.start, at) };
        clone.length -= at;
        self.length = at;
        clone
    }

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
        self.start = unsafe { non_null_add(self.start, at) };
        self.length -= at;
        clone
    }

    #[inline]
    pub fn try_into_mut(mut self) -> Result<ArcSliceMut<T>, Self> {
        let mut slice_mut = match self.inner_mut() {
            Inner::Static => return Err(self),
            Inner::Vec { capacity } => ArcSliceMut::new(unsafe { self.rebuild_vec(capacity) }),
            Inner::Arc(mut arc) => match unsafe { arc.try_as_mut() } {
                Some(s) => s,
                None => return Err(self),
            },
        };
        unsafe { slice_mut.set_start_len(self.start, self.length) };
        mem::forget(self);
        Ok(slice_mut)
    }

    #[inline]
    pub fn into_vec(self) -> Vec<T>
    where
        T: Clone,
    {
        let mut this = ManuallyDrop::new(self);
        match this.inner_mut() {
            Inner::Static => this.as_slice().to_vec(),
            Inner::Vec { capacity } => unsafe { this.shift_vec(this.rebuild_vec(capacity)) },
            Inner::Arc(mut arc) => unsafe {
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
    pub fn into_cow(mut self) -> Cow<'static, [T]>
    where
        T: Clone,
    {
        match self.inner_mut() {
            Inner::Static => unsafe {
                mem::transmute::<&[T], &'static [T]>(self.as_slice()).into()
            },
            _ => self.into_vec().into(),
        }
    }

    #[inline]
    pub fn get_metadata<M: Any>(&self) -> Option<&M> {
        match self.inner(self.arc_or_capa.load(Ordering::Acquire)) {
            Inner::Arc(arc) => arc.get_metadata(),
            _ if is!(M, ()) => Some(unit_metadata()),
            _ => None,
        }
    }

    #[inline]
    pub fn downcast_buffer<B: Buffer<T>>(mut self) -> Result<B, Self> {
        let mut buffer = MaybeUninit::<B>::uninit();
        match self.inner_mut() {
            Inner::Static if is!(B, &'static [T]) => unsafe {
                buffer.as_mut_ptr().cast::<&[T]>().write(self.as_slice());
            },
            Inner::Vec { capacity } if is!(B, Vec<T>) => unsafe {
                let vec_ptr = buffer.as_mut_ptr().cast::<Vec<T>>();
                vec_ptr.write(self.shift_vec(self.rebuild_vec(capacity)));
            },
            Inner::Arc(mut arc) => unsafe {
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

    #[inline]
    pub fn is_unique(&self) -> bool {
        match self.inner(self.arc_or_capa.load(Ordering::Acquire)) {
            Inner::Static => false,
            Inner::Vec { .. } => true,
            Inner::Arc(arc) => arc.is_unique(),
        }
    }

    #[inline]
    pub fn with_layout<L2: Layout>(self) -> ArcSlice<T, L2> {
        let mut this = ManuallyDrop::new(self);
        let arc_or_capa = atomic_ptr_with_mut(&mut this.arc_or_capa, |ptr| *ptr);
        match this.inner(arc_or_capa) {
            Inner::Vec { capacity } => ArcSlice::new_vec(unsafe { this.rebuild_vec(capacity) }),
            _ => ArcSlice {
                arc_or_capa: arc_or_capa.into(),
                base: MaybeUninit::uninit(),
                start: this.start,
                length: this.length,
            },
        }
    }

    #[cold]
    unsafe fn drop_vec(&mut self) {
        let Inner::Vec { capacity } = self.inner_mut() else {
            unsafe { hint::unreachable_unchecked() }
        };
        drop(unsafe { self.rebuild_vec(capacity) });
    }

    #[cold]
    unsafe fn clone_vec(&self, arc_or_capa: *mut ()) -> Self {
        let Inner::Vec { capacity } = self.inner(arc_or_capa) else {
            unsafe { hint::unreachable_unchecked() }
        };
        let vec = unsafe { self.rebuild_vec(capacity) };
        let (arc, _, _) = Arc::new(vec, (), 2);
        let arc_ptr = arc.into_ptr();
        // Release ordering must be used to ensure the arc vtable is visible
        // by `get_metadata`. In case of failure, the read arc is cloned with
        // a FAA, so there is no need of synchronization.
        let arc = match self.arc_or_capa.compare_exchange(
            arc_or_capa,
            arc_ptr.as_ptr(),
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => unsafe { Arc::from_ptr(arc_ptr) },
            Err(ptr) => {
                unsafe { Arc::<T>::from_ptr(arc_ptr).forget_vec() };
                let arc = unsafe { Arc::from_ptr(NonNull::new(ptr).unwrap_unchecked()) };
                (*ManuallyDrop::new(arc)).clone()
            }
        };
        unsafe { Self::from_arc(arc, self.start, self.length) }
    }
}

unsafe impl<T: Send + Sync + 'static, L: Layout> Send for ArcSlice<T, L> {}
unsafe impl<T: Send + Sync + 'static, L: Layout> Sync for ArcSlice<T, L> {}

impl<T: Send + Sync + 'static, L: Layout> Drop for ArcSlice<T, L> {
    #[inline]
    fn drop(&mut self) {
        match self.inner_mut() {
            Inner::Static => {}
            Inner::Vec { .. } => unsafe { self.drop_vec() },
            Inner::Arc(arc) => drop(ManuallyDrop::into_inner(arc)),
        }
    }
}

impl<T: Send + Sync + 'static, L: Layout> Clone for ArcSlice<T, L> {
    #[inline]
    fn clone(&self) -> Self {
        let arc_or_capa = self.arc_or_capa.load(Ordering::Acquire);
        match self.inner(arc_or_capa) {
            Inner::Static => {}
            Inner::Vec { .. } => return unsafe { self.clone_vec(arc_or_capa) },
            Inner::Arc(arc) => {
                let _ = arc.clone();
            }
        };
        Self {
            arc_or_capa: AtomicPtr::new(arc_or_capa),
            base: MaybeUninit::uninit(),
            start: self.start,
            length: self.length,
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

#[cfg(not(all(loom, test)))]
impl<T: Send + Sync + 'static, L: Layout> Default for ArcSlice<T, L> {
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
        for &b in self.as_slice() {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl<L: Layout> fmt::UpperHex for ArcSlice<u8, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.as_slice() {
            write!(f, "{:02X}", b)?;
        }
        Ok(())
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

impl<T: Send + Sync + 'static> From<ArcSlice<T, Compact>> for ArcSlice<T, Plain> {
    fn from(value: ArcSlice<T, Compact>) -> Self {
        value.with_layout()
    }
}

impl<T: Send + Sync + 'static> From<ArcSlice<T, Plain>> for ArcSlice<T, Compact> {
    fn from(value: ArcSlice<T, Plain>) -> Self {
        value.with_layout()
    }
}

macro_rules! std_impl {
    ($($(@$N:ident)? $ty:ty $(: $bound:path)?),*) => {$(
        impl<T: $($bound +)? Send + Sync + 'static, L: Layout, $(const $N: usize,)?> From<$ty> for ArcSlice<T, L> {

            #[inline]
            fn from(value: $ty) -> Self {
                Self::new(value)
            }
        }
    )*};
}
std_impl!(&'static [T], @N &'static [T; N], @N [T; N], Box<[T]>, Vec<T>, Cow<'static, [T]>: Clone);

impl<T: Clone + Send + Sync + 'static, L: Layout> From<ArcSlice<T, L>> for Vec<T> {
    #[inline]
    fn from(value: ArcSlice<T, L>) -> Self {
        value.into_vec()
    }
}

impl<T: Clone + Send + Sync + 'static, L: Layout> From<ArcSlice<T, L>> for Cow<'static, [T]> {
    #[inline]
    fn from(value: ArcSlice<T, L>) -> Self {
        value.into_cow()
    }
}

#[derive(Clone, Copy)]
pub struct ArcSliceRef<'a, T: Send + Sync + 'static, L: Layout = Compact> {
    slice: &'a [T],
    arc_slice: &'a ArcSlice<T, L>,
}

impl<T: Send + Sync + 'static, L: Layout> Deref for ArcSliceRef<'_, T, L> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.slice
    }
}

impl<T: fmt::Debug + Send + Sync + 'static, L: Layout> fmt::Debug for ArcSliceRef<'_, T, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.slice.fmt(f)
    }
}

impl<T: Send + Sync + 'static, L: Layout> ArcSliceRef<'_, T, L> {
    #[inline]
    pub fn into_arc(self) -> ArcSlice<T, L> {
        let (offset, len) = unsafe { offset_len_subslice_unchecked(self.arc_slice, self.slice) };
        unsafe { self.arc_slice.subslice_impl(offset, len) }
    }
}
