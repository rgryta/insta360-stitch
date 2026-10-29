//! The native bridge of the Kotlin and Java bindings. The JNI functions are for Android and the JVM. The C API is
//! for iOS (Kotlin/Native cinterop). Both call the same functions in this file, thus they agree.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use insta360_stitch as core;

#[cfg(not(target_os = "ios"))]
mod jni;

/// An open Insta360 file. Kotlin keeps a pointer to it, and frees it one time.
pub struct Handle {
    #[expect(dead_code, reason = "the preview reads it (next change)")]
    source: Source,
    stitcher: Arc<core::Stitcher>,
}

/// Where the preview is read from.
enum Source {
    Path(#[expect(dead_code, reason = "the preview reads it (next change)")] String),
}

/// The result of a bridge call: a core error or a panic. JNI and C map it to their own errors.
pub enum Failure {
    Core(core::Error),
    Panic,
}

impl From<core::Error> for Failure {
    fn from(error: core::Error) -> Self {
        Failure::Core(error)
    }
}

/// Runs `f`. If it panics, the panic becomes `Failure::Panic`. A panic must not cross the FFI boundary.
fn guarded<T>(f: impl FnOnce() -> Result<T, Failure>) -> Result<T, Failure> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(Err(Failure::Panic))
}

impl Handle {
    /// Reads the trailer of the file at `path`.
    fn open(path: String) -> Result<Handle, Failure> {
        let file = File::open(&path).map_err(core::Error::Io)?;
        let trailer = core::read_trailer(&mut BufReader::new(file))?;
        Ok(Handle {
            source: Source::Path(path),
            stitcher: Arc::new(core::Stitcher::new(trailer)),
        })
    }

    fn unsupported_reason(&self) -> Option<&str> {
        self.stitcher.unsupported_reason()
    }

    fn local_time(&self) -> Option<&str> {
        self.stitcher.trailer.local_time.as_deref()
    }

    fn offset_state(&self) -> i32 {
        self.stitcher.trailer.offset_state as i32
    }

    fn preview_size(&self) -> Option<(u32, u32)> {
        self.stitcher.trailer.preview_size()
    }
}
