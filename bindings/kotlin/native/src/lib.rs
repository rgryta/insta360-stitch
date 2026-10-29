//! The native bridge of the Kotlin and Java bindings. The JNI functions are for Android and the JVM. The C API is
//! for iOS (Kotlin/Native cinterop). Both call the same functions in this file, thus they agree.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use insta360_stitch as core;

#[cfg(target_os = "android")]
mod bitmap;
mod capi;
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
    /// A duplicated file descriptor (Android `content://` files). It shares the file position with the descriptor of
    /// the app, thus the bridge reads it only with positioned reads (`PositionedReader`).
    #[cfg_attr(
        not(all(unix, not(target_os = "ios"))),
        expect(dead_code, reason = "only JNI on Unix systems gives a file descriptor")
    )]
    File(File),
}

/// Reads a file with positioned reads (`pread`) and an own position. The file position of the descriptor does not
/// change: an app that reads the same descriptor (for example with `BitmapFactory.decodeFileDescriptor`) is not
/// disturbed, and two readers do not need a lock.
#[cfg(all(unix, not(target_os = "ios")))]
struct PositionedReader<'a> {
    file: &'a File,
    position: u64,
}

#[cfg(all(unix, not(target_os = "ios")))]
impl std::io::Read for PositionedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::unix::fs::FileExt;
        let read = self.file.read_at(buf, self.position)?;
        self.position += read as u64;
        Ok(read)
    }
}

#[cfg(all(unix, not(target_os = "ios")))]
impl std::io::Seek for PositionedReader<'_> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        let (base, offset) = match to {
            SeekFrom::Start(position) => (0, position as i128),
            SeekFrom::Current(offset) => (self.position, offset as i128),
            SeekFrom::End(offset) => (self.file.metadata()?.len(), offset as i128),
        };
        let position = u64::try_from(base as i128 + offset).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            )
        })?;
        self.position = position;
        Ok(position)
    }
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
    #[cfg(all(unix, not(target_os = "ios")))]
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
        let reader = PositionedReader {
            file: &file,
            position: 0,
        };
        let trailer = core::read_trailer(&mut BufReader::new(reader))?;
        Ok(Handle {
            source: Source::File(file),
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
            #[cfg(all(unix, not(target_os = "ios")))]
            Source::File(file) => {
                let reader = PositionedReader { file, position: 0 };
                core::read_preview(&mut BufReader::new(reader), &self.stitcher.trailer)?
            }
            #[cfg(not(all(unix, not(target_os = "ios"))))]
            Source::File(_) => unreachable!("only JNI on Unix systems gives a file descriptor"),
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

#[cfg(all(test, all(unix, not(target_os = "ios"))))]
mod tests {
    use std::io::{Seek, SeekFrom};
    use std::os::fd::AsRawFd;

    use super::Handle;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/synthetic_x5.insp"
    );

    /// The app reads the same descriptor: open and preview must not move its file position.
    #[test]
    fn open_fd_does_not_move_the_file_position() {
        let mut file = std::fs::File::open(FIXTURE).unwrap();
        file.seek(SeekFrom::Start(100)).unwrap();
        let handle = Handle::open_fd(file.as_raw_fd()).ok().unwrap();
        assert_eq!(file.stream_position().unwrap(), 100);
        assert_eq!(handle.preview().ok().unwrap().0, 2);
        assert_eq!(file.stream_position().unwrap(), 100);
    }
}
