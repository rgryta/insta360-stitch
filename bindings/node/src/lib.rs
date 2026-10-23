//! The Node.js bindings. A trailer read reads only the records that it needs.
//! The stitch operations run on the libuv thread pool. Thus, they do not stop the event loop.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use insta360_stitch as core;
use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Runs `f`. If it panics, the panic becomes a JS error. Without this, a panic stops the Node.js process.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Error::from_reason("internal error: the stitcher panicked")))
}

/// Each error starts with "unsupported: ", "i/o error: " or "invalid argument: ".
/// "unsupported" means that the library cannot stitch the file. "i/o error" means that a retry can help.
fn to_js(error: core::Error) -> Error {
    Error::from_reason(error.to_string())
}

/// A width and a height in pixels.
#[napi(object)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// The preview that the camera stitched.
#[napi(object)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    /// The image data as packed RGB.
    pub rgb: Buffer,
}

/// The remap planes for ffmpeg. The output is `width` x `height`.
#[napi(object)]
pub struct VideoMaps {
    pub width: u32,
    pub height: u32,
    /// Little-endian `u16` planes (`gray16le`). The value 65535 is outside that lens.
    pub x0: Buffer,
    pub y0: Buffer,
    pub x1: Buffer,
    pub y1: Buffer,
    /// The blend weight of lens 0 (`gray`).
    pub mask: Buffer,
}

/// An Insta360 .insp or .insv file. The open operation reads its trailer and calculates the level rotation one time.
/// The async tasks share the stitcher, thus a task does not copy the IMU data or calculate the rotation again.
#[napi]
pub struct Insta360File {
    path: String,
    stitcher: Arc<core::Stitcher>,
}

#[napi]
impl Insta360File {
    /// Reads the trailer. It reads only the records that the stitcher needs, and not the image or video data.
    #[napi(factory)]
    pub fn open(path: String) -> Result<Self> {
        guarded(|| {
            let file = File::open(&path).map_err(|e| to_js(core::Error::Io(e)))?;
            let trailer = core::read_trailer(&mut BufReader::new(file)).map_err(to_js)?;
            Ok(Insta360File {
                path,
                stitcher: Arc::new(core::Stitcher::new(trailer)),
            })
        })
    }

    /// Is `true` if the library supports the calibration, the lens guard and the IMU data.
    #[napi(getter)]
    pub fn stitchable(&self) -> bool {
        self.stitcher.unsupported_reason().is_none()
    }

    /// The reason why the library cannot stitch the file, or null.
    #[napi(getter)]
    pub fn unsupported_reason(&self) -> Option<String> {
        self.stitcher.unsupported_reason().map(String::from)
    }

    /// The time on the camera clock when the camera made the file, as `YYYYMMDDhhmmss`.
    #[napi(getter)]
    pub fn local_time(&self) -> Option<String> {
        self.stitcher.trailer.local_time.clone()
    }

    /// The state of the lens guard or accessory: 0 = none, 5 = premium guard.
    #[napi(getter)]
    pub fn offset_state(&self) -> u32 {
        self.stitcher.trailer.offset_state as u32
    }

    /// The width and height of the preview, or null if the trailer has no preview.
    #[napi(getter)]
    pub fn preview_size(&self) -> Option<Size> {
        self.stitcher
            .trailer
            .preview_size()
            .map(|(width, height)| Size { width, height })
    }

    /// Gives the small equirectangular preview that the camera stitched.
    #[napi]
    pub fn preview(&self) -> Result<PreviewImage> {
        guarded(|| {
            let file = File::open(&self.path).map_err(|e| to_js(core::Error::Io(e)))?;
            let preview = core::read_preview(&mut BufReader::new(file), &self.stitcher.trailer)
                .map_err(to_js)?;
            Ok(PreviewImage {
                width: preview.width,
                height: preview.height,
                rgb: core::preview_rgb(&preview).into(),
            })
        })
    }

    /// Stitches the decoded raw pixels of the photo into an equirectangular image.
    /// The two lenses are side by side (`width = 2 * height`). The output has the same size and channels.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn stitch_photo(
        &self,
        raw: Buffer,
        width: u32,
        height: u32,
        channels: u32,
    ) -> AsyncTask<StitchPhoto> {
        AsyncTask::new(StitchPhoto {
            stitcher: self.stitcher.clone(),
            raw,
            width,
            height,
            channels,
            band: None,
        })
    }

    /// Stitches only the output rows `firstRow` to `firstRow + rows - 1` of the photo.
    /// Use it to stitch a photo in parts, for example to show progress or to limit memory.
    /// The arguments `raw`, `width`, `height` and `channels` are the same as for `stitchPhoto`.
    #[napi(ts_return_type = "Promise<Buffer>")]
    #[allow(clippy::too_many_arguments)]
    pub fn stitch_photo_band(
        &self,
        raw: Buffer,
        width: u32,
        height: u32,
        channels: u32,
        first_row: u32,
        rows: u32,
    ) -> AsyncTask<StitchPhoto> {
        AsyncTask::new(StitchPhoto {
            stitcher: self.stitcher.clone(),
            raw,
            width,
            height,
            channels,
            band: Some((first_row, rows)),
        })
    }

    /// Makes the remap planes for a .insv file with lens streams of `lensWidth` x `lensHeight`.
    /// The output is `width` x `width / 2`. The default width is `2 * lensWidth`, which keeps the resolution of the lenses.
    /// `width` must be a multiple of 4 and at most 16384.
    /// Give the planes to ffmpeg with `FFMPEG_FILTER`. At a width of 7680, the planes use approximately 265 MB.
    #[napi(ts_return_type = "Promise<VideoMaps>")]
    pub fn video_maps(
        &self,
        lens_width: u32,
        lens_height: u32,
        width: Option<u32>,
    ) -> AsyncTask<BuildVideoMaps> {
        AsyncTask::new(BuildVideoMaps {
            stitcher: self.stitcher.clone(),
            width,
            lens_width,
            lens_height,
        })
    }
}

pub struct StitchPhoto {
    stitcher: Arc<core::Stitcher>,
    raw: Buffer,
    width: u32,
    height: u32,
    channels: u32,
    /// The first row and the number of rows, or `None` for the full photo.
    band: Option<(u32, u32)>,
}

impl Task for StitchPhoto {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        guarded(|| {
            let (width, height, channels) = (
                self.width as usize,
                self.height as usize,
                self.channels as usize,
            );
            let source = core::Image {
                width,
                height,
                channels,
                data: &self.raw,
            };
            let band = self
                .band
                .map(|(first_row, rows)| (first_row as usize, rows as usize));
            self.stitcher.photo(&source, band).map_err(to_js)
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

pub struct BuildVideoMaps {
    stitcher: Arc<core::Stitcher>,
    width: Option<u32>,
    lens_width: u32,
    lens_height: u32,
}

impl Task for BuildVideoMaps {
    /// The width, the height, the x0, y0, x1 and y1 planes as bytes, and the mask.
    /// `compute` converts the planes, thus `resolve` on the event loop does not copy data.
    type Output = (usize, usize, [Vec<u8>; 4], Vec<u8>);
    type JsValue = VideoMaps;

    fn compute(&mut self) -> Result<Self::Output> {
        guarded(|| {
            let maps = self
                .stitcher
                .video_maps(
                    self.lens_width as usize,
                    self.lens_height as usize,
                    self.width.map(|width| width as usize),
                )
                .map_err(to_js)?;
            Ok((maps.width, maps.height, maps.le_bytes(), maps.mask))
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        let (width, height, planes, mask) = output;
        let [x0, y0, x1, y1] = planes.map(Buffer::from);
        Ok(VideoMaps {
            width: width as u32,
            height: height as u32,
            x0,
            y0,
            x1,
            y1,
            mask: mask.into(),
        })
    }
}

pub struct SphericalMp4 {
    input: String,
    output: String,
}

impl Task for SphericalMp4 {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        guarded(|| {
            core::write_spherical_mp4_file(self.input.as_ref(), self.output.as_ref()).map_err(to_js)
        })
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

/// Copies the MP4 file at `input` to `output` and adds the 360 video metadata (Spherical Video V1 and V2).
/// It also moves the `moov` box to the start, the same as ffmpeg `-movflags +faststart`.
/// Give ffmpeg an output without `+faststart`, then use this function. `output` must be a different file.
/// The function writes a temporary file next to `output` and renames it. Thus, after an error, `output` did not change.
/// It refuses a fragmented MP4 file.
#[napi(ts_return_type = "Promise<void>")]
pub fn add_spherical_metadata(input: String, output: String) -> AsyncTask<SphericalMp4> {
    AsyncTask::new(SphericalMp4 { input, output })
}

/// The ffmpeg `-filter_complex` for `videoMaps`. Input 0 is the .insv file.
/// Inputs 1 to 4 are x0, y0, x1 and y1 (`gray16le`). Input 5 is the mask (`gray`).
#[napi]
pub const FFMPEG_FILTER: &str = core::FFMPEG_FILTER;

/// Gives the UTC offset of the camera in minutes, from the camera clock and the same time in UTC.
/// It gives null if the difference is not a real time zone. In that condition, the camera clock is wrong.
#[napi]
pub fn camera_utc_offset_minutes(local_time: String, utc_unix_seconds: i64) -> Option<i32> {
    core::camera_utc_offset_minutes(&local_time, utc_unix_seconds)
}
