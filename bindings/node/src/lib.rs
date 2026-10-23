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

/// Each error starts with "unsupported: " or "i/o error: ".
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

/// The trailer and its level rotation. The open operation calculates the rotation one time.
/// The async tasks share them, thus a task does not copy the IMU data or calculate the rotation again.
#[derive(Clone)]
struct Shared {
    trailer: Arc<core::Trailer>,
    level: Arc<std::result::Result<core::math::Mat3, String>>,
}

impl Shared {
    fn model(&self, lens_size: usize) -> core::Result<core::LensModel> {
        match self.level.as_ref() {
            Ok(level) => core::LensModel::new(&self.trailer, lens_size, *level),
            Err(reason) => Err(core::Error::Unsupported(reason.clone())),
        }
    }
}

/// Calculates the level rotation. An error keeps only its reason, because the orientation gives only "unsupported" errors.
fn level_of(trailer: &core::Trailer) -> std::result::Result<core::math::Mat3, String> {
    core::orientation(trailer).map_err(|error| match error {
        core::Error::Unsupported(reason) => reason,
        other => other.to_string(),
    })
}

/// Gives the reason why the library cannot stitch the trailer, or `None`.
fn unsupported_reason(
    trailer: &core::Trailer,
    level: &std::result::Result<core::math::Mat3, String>,
) -> Option<String> {
    match level {
        Err(reason) => Some(reason.clone()),
        Ok(level) => match core::LensModel::new(trailer, 1, *level) {
            Ok(_) => None,
            Err(core::Error::Unsupported(reason)) => Some(reason),
            Err(other) => Some(other.to_string()),
        },
    }
}

/// An Insta360 .insp or .insv file. The open operation reads its trailer.
#[napi]
pub struct Insta360File {
    path: String,
    shared: Shared,
    unsupported: Option<String>,
}

#[napi]
impl Insta360File {
    /// Reads the trailer. It reads only the records that the stitcher needs, and not the image or video data.
    #[napi(factory)]
    pub fn open(path: String) -> Result<Self> {
        guarded(|| {
            let file = File::open(&path).map_err(|e| to_js(core::Error::Io(e)))?;
            let trailer = core::read_trailer(&mut BufReader::new(file)).map_err(to_js)?;
            let level = level_of(&trailer);
            let unsupported = unsupported_reason(&trailer, &level);
            Ok(Insta360File {
                path,
                shared: Shared {
                    trailer: Arc::new(trailer),
                    level: Arc::new(level),
                },
                unsupported,
            })
        })
    }

    /// Is `true` if the library supports the calibration, the lens guard and the IMU data.
    #[napi(getter)]
    pub fn stitchable(&self) -> bool {
        self.unsupported.is_none()
    }

    /// The reason why the library cannot stitch the file, or null.
    #[napi(getter)]
    pub fn unsupported_reason(&self) -> Option<String> {
        self.unsupported.clone()
    }

    /// The time on the camera clock when the camera made the file, as `YYYYMMDDhhmmss`.
    #[napi(getter)]
    pub fn local_time(&self) -> Option<String> {
        self.shared.trailer.local_time.clone()
    }

    /// The state of the lens guard or accessory: 0 = none, 5 = premium guard.
    #[napi(getter)]
    pub fn offset_state(&self) -> u32 {
        self.shared.trailer.offset_state as u32
    }

    /// The width and height of the preview, or null if the trailer has no preview.
    #[napi(getter)]
    pub fn preview_size(&self) -> Option<Size> {
        self.shared
            .trailer
            .preview_size()
            .map(|(width, height)| Size { width, height })
    }

    /// Gives the small equirectangular preview that the camera stitched.
    #[napi]
    pub fn preview(&self) -> Result<PreviewImage> {
        guarded(|| {
            let file = File::open(&self.path).map_err(|e| to_js(core::Error::Io(e)))?;
            let preview = core::read_preview(&mut BufReader::new(file), &self.shared.trailer)
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
            shared: self.shared.clone(),
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
            shared: self.shared.clone(),
            raw,
            width,
            height,
            channels,
            band: Some((first_row, rows)),
        })
    }

    /// Makes the remap planes for a .insv file with lens streams of `lensWidth` x `lensHeight`.
    /// The output is `width` x `width / 2`. The default width is `2 * lensWidth`, which keeps the resolution of the lenses.
    /// Give the planes to ffmpeg with `FFMPEG_FILTER`. At a width of 7680, the planes use approximately 265 MB.
    #[napi(ts_return_type = "Promise<VideoMaps>")]
    pub fn video_maps(
        &self,
        lens_width: u32,
        lens_height: u32,
        width: Option<u32>,
    ) -> AsyncTask<BuildVideoMaps> {
        AsyncTask::new(BuildVideoMaps {
            shared: self.shared.clone(),
            width: width.unwrap_or(lens_width.saturating_mul(2)),
            lens_width,
            lens_height,
        })
    }
}

pub struct StitchPhoto {
    shared: Shared,
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
            let model = self.shared.model(height).map_err(to_js)?;
            let source = core::Image {
                width,
                height,
                channels,
                data: &self.raw,
            };
            match self.band {
                None => core::stitch_photo(&source, &model),
                Some((first_row, rows)) => {
                    // Check the range before the allocation. A very large `rows` must not stop the process.
                    if u64::from(first_row) + u64::from(rows) > height as u64 {
                        return Err(to_js(core::Error::InvalidArgument(format!(
                            "rows {first_row}..{} are outside the photo",
                            u64::from(first_row) + u64::from(rows)
                        ))));
                    }
                    let mut out = vec![0; rows as usize * width * channels];
                    core::stitch_photo_band(&source, &model, first_row as usize, &mut out)
                        .map(|()| out)
                }
            }
            .map_err(to_js)
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

pub struct BuildVideoMaps {
    shared: Shared,
    width: u32,
    lens_width: u32,
    lens_height: u32,
}

fn le_bytes(plane: &[u16]) -> Buffer {
    plane
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<u8>>()
        .into()
}

impl Task for BuildVideoMaps {
    type Output = core::VideoMaps;
    type JsValue = VideoMaps;

    fn compute(&mut self) -> Result<Self::Output> {
        guarded(|| {
            let (width, lens_width) = (self.width as usize, self.lens_width as usize);
            let model = self.shared.model(lens_width).map_err(to_js)?;
            core::video_maps(&model, width, lens_width, self.lens_height as usize).map_err(to_js)
        })
    }

    fn resolve(&mut self, _env: Env, maps: Self::Output) -> Result<Self::JsValue> {
        Ok(VideoMaps {
            width: maps.width as u32,
            height: maps.height as u32,
            x0: le_bytes(&maps.x[0]),
            y0: le_bytes(&maps.y[0]),
            x1: le_bytes(&maps.x[1]),
            y1: le_bytes(&maps.y[1]),
            mask: maps.mask.into(),
        })
    }
}

/// Refuses an output path that is the input file. Creating the output would delete the input before the read.
fn check_different_files(input: &std::path::Path, output: &std::path::Path) -> core::Result<()> {
    let same = match (input.canonicalize(), output.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => input == output,
    };
    if same {
        return Err(core::Error::InvalidArgument(
            "the output must be a different file than the input".into(),
        ));
    }
    Ok(())
}

/// Copies the MP4 file at `input` to `output` and adds the 360 video metadata. See `write_spherical_mp4`.
fn spherical_mp4(input: &std::path::Path, output: &std::path::Path) -> core::Result<()> {
    use std::io::Write;
    check_different_files(input, output)?;
    let mut source = BufReader::new(File::open(input)?);
    let mut target = std::io::BufWriter::new(File::create(output)?);
    core::write_spherical_mp4(&mut source, &mut target)?;
    target.flush()?;
    Ok(())
}

pub struct SphericalMp4 {
    input: String,
    output: String,
}

impl Task for SphericalMp4 {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        guarded(|| spherical_mp4(self.input.as_ref(), self.output.as_ref()).map_err(to_js))
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

/// Copies the MP4 file at `input` to `output` and adds the 360 video metadata (Spherical Video V1 and V2).
/// It also moves the `moov` box to the start, the same as ffmpeg `-movflags +faststart`.
/// Give ffmpeg an output without `+faststart`, then use this function. `output` must be a different file.
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
