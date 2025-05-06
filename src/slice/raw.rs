use alloc::vec::Vec;
use core::{
    any::{Any, TypeId},
    mem::{ManuallyDrop, MaybeUninit},
    ptr::NonNull,
};

use crate::{
    atomic::AtomicPtr,
    buffer::{Buffer, BufferWithMetadata, DynBuffer, RawBuffer},
    layout::{Layout, RawLayout},
    msrv::ptr,
    slice::ArcSliceLayout,
    ArcSlice,
};

#[allow(missing_debug_implementations)]
pub union RawPtr {
    raw: *mut (),
    atomic_ptr: ManuallyDrop<AtomicPtr<()>>,
}

type ArcSlicePtr = NonNull<()>;
type ArcSliceMutPtr = NonNull<()>;
type ArcSliceOrRawPtr = *mut ();
type ArcSliceOrDataPtr = *mut ();

#[allow(missing_debug_implementations)]
pub struct VTable {
    // mimic return-by-value calling convention (take return ptr as first arg and return it)
    clone: unsafe fn(ArcSlicePtr, ArcSlicePtr) -> ArcSlicePtr,
    drop: unsafe fn(ArcSliceOrRawPtr),
    is_unique: unsafe fn(ArcSliceOrDataPtr) -> bool,
    get_metadata: Option<unsafe fn(ArcSliceOrDataPtr, TypeId) -> Option<NonNull<()>>>,
    try_into_buffer: unsafe fn(ArcSlicePtr, TypeId, NonNull<()>),
    try_into_mut: unsafe fn(ArcSliceMutPtr, ArcSlicePtr) -> Option<ArcSliceMutPtr>,
    try_into_mut_vec: unsafe fn(ArcSliceMutPtr, ArcSlicePtr) -> Option<ArcSliceMutPtr>,
}

struct Data {
    ptr: RawPtr,
    vtable: &'static VTable,
}

impl<const BOXED_SLICE: bool> ArcSliceLayout for RawLayout<BOXED_SLICE> {
    type Data = Data;
    const STATIC_DATA: Option<Self::Data> = Some(Data {
        ptr: RawPtr {
            raw: ptr::null_mut(),
        },
        vtable: panic!(),
    });
    fn new_slice<T: Send + Sync + 'static>(slice: &[T]) -> ArcSlice<T, Self> {
        todo!()
    }
    fn new_buffer<T: Send + Sync + 'static, B: DynBuffer + Buffer<T>>(
        buffer: B,
    ) -> ArcSlice<T, Self> {
        todo!()
    }
    fn new_static<T: Send + Sync + 'static>(slice: &'static [T]) -> ArcSlice<T, Self> {
        ArcSlice::new_static_impl(slice)
    }
    fn new_vec<T: Send + Sync + 'static>(mut vec: Vec<T>) -> ArcSlice<T, Self> {
        if BOXED_SLICE && vec.len() == vec.capacity() {
            let data = Data {
                ptr: RawPtr {
                    raw: vec.as_mut_ptr().cast(),
                },
                vtable: panic!(),
            };
            return ArcSlice::new_vec_impl(data, vec);
        }
        Self::new_buffer(BufferWithMetadata::new(vec, ()))
    }
    fn new_raw_buffer<T: Send + Sync + 'static, B: DynBuffer + RawBuffer<T>>(
        buffer: B,
    ) -> ArcSlice<T, Self> {
        let slice = buffer.as_slice();
        ArcSlice {
            start: NonNull::new(slice.as_ptr().cast_mut()).unwrap(),
            length: slice.len(),
            data: Data {
                ptr: RawPtr {
                    raw: buffer.into_raw(),
                },
                vtable: panic!(),
            },
        }
    }

    fn clone<T: Send + Sync + 'static>(slice: &ArcSlice<T, Self>) -> ArcSlice<T, Self> {
        let mut clone = MaybeUninit::uninit();
        unsafe {
            (slice.data.vtable.clone)(
                NonNull::from(&mut clone).cast(),
                NonNull::from(slice).cast(),
            );
        }
        unsafe { clone.assume_init() }
    }

    fn drop<T: Send + Sync + 'static>(slice: &mut ArcSlice<T, Self>) {
        let ptr = if BOXED_SLICE {
            ptr::from_mut(slice).cast()
        } else {
            unsafe { slice.data.ptr.raw }
        };
        unsafe { (slice.data.vtable.drop)(ptr) }
    }

    fn truncate<T: Send + Sync + 'static>(_slice: &mut ArcSlice<T, Self>, _len: usize)
    where
        Self: Layout + Sized,
    {
        if BOXED_SLICE {}
    }

    fn is_unique(data: &Self::Data) -> bool {
        let ptr = if BOXED_SLICE {
            ptr::from_ref(data).cast_mut().cast()
        } else {
            unsafe { data.ptr.raw }
        };
        unsafe { (data.vtable.is_unique)(ptr) }
    }

    fn get_metadata<M: Any>(data: &Self::Data) -> Option<&M> {
        let ptr = if BOXED_SLICE {
            ptr::from_ref(data).cast_mut().cast()
        } else {
            unsafe { data.ptr.raw }
        };
        let type_id = TypeId::of::<M>();
        Some(unsafe { data.vtable.get_metadata?(ptr, type_id)?.cast().as_ref() })
    }
}
