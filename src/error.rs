use core::{fmt, str::Utf8Error};

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

pub struct FromUtf8Error<B> {
    pub(crate) bytes: B,
    pub(crate) error: Utf8Error,
}

impl<B> FromUtf8Error<B> {
    pub fn as_bytes(&self) -> &B {
        &self.bytes
    }

    pub fn into_bytes(self) -> B {
        self.bytes
    }

    pub fn error(&self) -> Utf8Error {
        self.error
    }
}

impl<B: fmt::Debug> fmt::Debug for FromUtf8Error<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FromUtf8Error")
            .field("bytes", &self.bytes)
            .field("error", &self.error)
            .finish()
    }
}

impl<B> fmt::Display for FromUtf8Error<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

#[cfg(feature = "std")]
const _: () = {
    extern crate std;
    impl<B: fmt::Debug> std::error::Error for FromUtf8Error<B> {}
};
