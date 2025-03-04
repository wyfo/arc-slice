use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TryReserveError {
    NotUnique,
    Unsupported,
}

impl fmt::Display for TryReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUnique => write!(f, "not unique"),
            Self::Unsupported => write!(f, "unsupported"),
        }
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;
    impl std::error::Error for TryReserveError {}
};
