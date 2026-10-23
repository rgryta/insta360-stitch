//! The Node.js bindings. A trailer read reads only the records that it needs.
//! The stitch operations run on the libuv thread pool. Thus, they do not stop the event loop.

use std::fs::File;
use std::io::BufReader;

use insta360_stitch as core;
use napi::bindgen_prelude::*;
use napi_derive::napi;

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

/// An Insta360 .insp or .insv file. The open operation reads its trailer.
#[napi]
pub struct Insta360File {
    path: String,
    trailer: core::Trailer,
    unsupported: Option<String>,
}

#[napi]
impl Insta360File {
    /// Reads the trailer. It reads only the records that the stitcher needs, and not the image or video data.
    #[napi(factory)]
    pub fn open(path: String) -> Result<Self> {
        let file = File::open(&path).map_err(|e| to_js(core::Error::Io(e)))?;
        let trailer = core::read_trailer(&mut BufReader::new(file)).map_err(to_js)?;
        let unsupported = match core::check_stitchable(&trailer) {
            Ok(()) => None,
            Err(core::Error::Unsupported(reason)) => Some(reason),
            Err(error) => return Err(to_js(error)),
        };
        Ok(Insta360File {
            path,
            trailer,
            unsupported,
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
        self.trailer.local_time.clone()
    }

    /// The state of the lens guard or accessory: 0 = none, 5 = premium guard.
    #[napi(getter)]
    pub fn offset_state(&self) -> u32 {
        self.trailer.offset_state as u32
    }

    /// The width and height of the preview, or null if the trailer has no preview.
    #[napi(getter)]
    pub fn preview_size(&self) -> Option<Size> {
        self.trailer
            .preview_size()
            .map(|(width, height)| Size { width, height })
    }

    /// Gives the small equirectangular preview that the camera stitched.
    #[napi]
    pub fn preview(&self) -> Result<PreviewImage> {
        let file = File::open(&self.path).map_err(|e| to_js(core::Error::Io(e)))?;
        let preview =
            core::read_preview(&mut BufReader::new(file), &self.trailer).map_err(to_js)?;
        Ok(PreviewImage {
            width: preview.width,
            height: preview.height,
            rgb: core::preview_rgb(&preview).into(),
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
            trailer: self.trailer.clone(),
            raw,
            width,
            height,
            channels,
        })
    }

    /// Makes the remap planes for a .insv file with lens streams of `width` x `lensHeight`.
    /// Give the planes to ffmpeg with `FFMPEG_FILTER`.
    #[napi(ts_return_type = "Promise<VideoMaps>")]
    pub fn video_maps(&self, width: u32, lens_height: u32) -> AsyncTask<BuildVideoMaps> {
        AsyncTask::new(BuildVideoMaps {
            trailer: self.trailer.clone(),
            width,
            lens_height,
        })
    }
}

pub struct StitchPhoto {
    trailer: core::Trailer,
    raw: Buffer,
    width: u32,
    height: u32,
    channels: u32,
}

impl Task for StitchPhoto {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        let (width, height, channels) = (
            self.width as usize,
            self.height as usize,
            self.channels as usize,
        );
        let level = core::orientation(&self.trailer).map_err(to_js)?;
        let model = core::LensModel::new(&self.trailer, height, level).map_err(to_js)?;
        core::stitch_photo(
            &core::Image {
                width,
                height,
                channels,
                data: &self.raw,
            },
            &model,
        )
        .map_err(to_js)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

pub struct BuildVideoMaps {
    trailer: core::Trailer,
    width: u32,
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
        let level = core::orientation(&self.trailer).map_err(to_js)?;
        let model =
            core::LensModel::new(&self.trailer, self.width as usize, level).map_err(to_js)?;
        core::video_maps(&model, self.width as usize, self.lens_height as usize).map_err(to_js)
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
