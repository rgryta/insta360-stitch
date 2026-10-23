use std::fmt;
use std::io;

/// The reason why the library cannot read or stitch a file.
#[derive(Debug)]
pub enum Error {
    /// The library cannot stitch this file, because of the camera, the lens accessory or the trailer layout.
    /// A retry will not help.
    Unsupported(String),
    /// The read operation failed, or it gave fewer bytes than necessary. A retry can help.
    Io(io::Error),
    /// The caller gave a wrong argument, for example a buffer of the wrong size or a band outside the photo.
    /// This is an error in the calling code, not in the file.
    InvalidArgument(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn unsupported<T>(reason: impl Into<String>) -> Result<T> {
    Err(Error::Unsupported(reason.into()))
}

pub(crate) fn invalid<T>(reason: impl Into<String>) -> Result<T> {
    Err(Error::InvalidArgument(reason.into()))
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(reason) => write!(f, "unsupported: {reason}"),
            Error::Io(error) => write!(f, "i/o error: {error}"),
            Error::InvalidArgument(reason) => write!(f, "invalid argument: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            Error::Unsupported(_) | Error::InvalidArgument(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}
