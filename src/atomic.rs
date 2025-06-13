#[cfg(not(feature = "portable-atomic"))]
pub(crate) use core::sync::atomic::*;

#[cfg(feature = "portable-atomic")]
pub(crate) use portable_atomic::*;
