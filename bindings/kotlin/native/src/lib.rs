//! The native bridge of the Kotlin and Java bindings. The JNI functions are for Android and the JVM. The C API is
//! for iOS (Kotlin/Native cinterop). Both call the same functions in this file, thus they agree.

use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, Mutex};

use insta360_stitch as core;

#[cfg(target_os = "android")]
mod bitmap;
#[cfg(not(target_os = "ios"))]
mod jni;

/// An open Insta360 file. Kotlin keeps a pointer to it, and frees it one time.
pub struct Handle {
    source: Source,
    stitcher: Arc<core::Stitcher>,
}

/// Where the preview is read from.
enum Source {
    Path(String),
    /// A duplicated file descriptor (Android `content://` files). The mutex keeps two reads apart.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only Unix systems give a file descriptor")
    )]
    File(Mutex<File>),
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

fn invalid(reason: impl Into<String>) -> Failure {
    Failure::Core(core::Error::InvalidArgument(reason.into()))
}

/// Converts a Kotlin `Int` to `usize`. A negative value is an error in the calling code.
fn size(value: i32, name: &str) -> Result<usize, Failure> {
    usize::try_from(value)
        .map_err(|_| invalid(format!("{name} must not be negative, it is {value}")))
}

/// The remap planes for ffmpeg: x0, y0, x1 and y1 as little-endian `u16`, then the mask.
pub struct Maps {
    pub width: usize,
    pub height: usize,
    pub planes: [Vec<u8>; 4],
    pub mask: Vec<u8>,
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

    /// Reads the trailer of an open file. The handle keeps its own duplicate of the descriptor, thus the caller can
    /// close its descriptor.
    #[cfg(unix)]
    fn open_fd(fd: i32) -> Result<Handle, Failure> {
        use std::os::fd::BorrowedFd;
        if fd < 0 {
            return Err(invalid(format!("the file descriptor {fd} is not valid")));
        }
        // SAFETY: the caller keeps `fd` open during this call. The duplicate does not depend on it after the call.
        let owned = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .map_err(core::Error::Io)?;
        let file = File::from(owned);
        let trailer = core::read_trailer(&mut BufReader::new(&file))?;
        Ok(Handle {
            source: Source::File(Mutex::new(file)),
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

    /// Gives the width, the height and the RGB data of the preview that the camera stitched.
    fn preview(&self) -> Result<(u32, u32, Vec<u8>), Failure> {
        let preview = match &self.source {
            Source::Path(path) => {
                let file = File::open(path).map_err(core::Error::Io)?;
                core::read_preview(&mut BufReader::new(file), &self.stitcher.trailer)?
            }
            Source::File(file) => {
                let file = file.lock().unwrap_or_else(|e| e.into_inner());
                core::read_preview(&mut BufReader::new(&*file), &self.stitcher.trailer)?
            }
        };
        Ok((preview.width, preview.height, core::preview_rgb(&preview)))
    }

    /// Stitches the rows `first_row` to `first_row + rows - 1` of the photo.
    fn stitch(
        &self,
        raw: &[u8],
        width: i32,
        height: i32,
        channels: i32,
        first_row: i32,
        rows: i32,
    ) -> Result<Vec<u8>, Failure> {
        let source = core::Image {
            width: size(width, "width")?,
            height: size(height, "height")?,
            channels: size(channels, "channels")?,
            data: raw,
        };
        let band = (size(first_row, "firstRow")?, size(rows, "rows")?);
        Ok(self.stitcher.photo(&source, Some(band))?)
    }

    /// Stitches the full photo `raw` (`width` x `height` x `channels`) directly into `out`, which has the same size.
    /// There is no copy: Android gives the pixels of its bitmaps, iOS gives a pinned array.
    #[cfg_attr(
        not(target_os = "android"),
        expect(dead_code, reason = "Android bitmaps and the C API use it")
    )]
    fn stitch_into(
        &self,
        raw: &[u8],
        width: usize,
        height: usize,
        channels: usize,
        out: &mut [u8],
    ) -> Result<(), Failure> {
        if out.len() != raw.len() {
            return Err(invalid(format!(
                "the output has {} bytes, the photo has {}",
                out.len(),
                raw.len()
            )));
        }
        let source = core::Image {
            width,
            height,
            channels,
            data: raw,
        };
        Ok(core::stitch_photo_band(
            &source,
            &self.stitcher.model(height)?,
            0,
            out,
        )?)
    }

    /// Makes the remap planes. `width` is `None` for the default width.
    fn video_maps(
        &self,
        lens_width: i32,
        lens_height: i32,
        width: Option<i32>,
    ) -> Result<Maps, Failure> {
        let width = width.map(|width| size(width, "width")).transpose()?;
        let maps = self.stitcher.video_maps(
            size(lens_width, "lensWidth")?,
            size(lens_height, "lensHeight")?,
            width,
        )?;
        let (width, height) = (maps.width, maps.height);
        let (planes, mask) = maps.into_le_bytes();
        Ok(Maps {
            width,
            height,
            planes,
            mask,
        })
    }
}

/// Copies the MP4 file at `input` to `output` and adds the 360 video metadata.
fn add_spherical_metadata(input: &str, output: &str) -> Result<(), Failure> {
    Ok(core::write_spherical_mp4_file(
        input.as_ref(),
        output.as_ref(),
    )?)
}
