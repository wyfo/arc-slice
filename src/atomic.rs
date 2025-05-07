#[cfg(all(feature = "portable-atomic-util", not(feature = "portable-atomic")))]
compile_error!("feature \"portable-atomic-util\" requires \"portable-atomic\"");

#[cfg(not(feature = "portable-atomic"))]
pub(crate) use core::sync::atomic::*;

#[cfg(feature = "portable-atomic")]
pub(crate) use portable_atomic::*;
