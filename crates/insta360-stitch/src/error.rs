use std::fmt;
use std::io;

/// Why a file could not be read or stitched.
#[derive(Debug)]
pub enum Error {
    /// The file itself cannot be stitched: another camera, lens accessory or trailer layout.
    /// Retrying will not help.
    Unsupported(String),
    /// Reading the file failed (including short reads); worth a retry.
    Io(io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn unsupported<T>(reason: impl Into<String>) -> Result<T> {
    Err(Error::Unsupported(reason.into()))
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(reason) => write!(f, "unsupported: {reason}"),
            Error::Io(error) => write!(f, "i/o error: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            Error::Unsupported(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}
