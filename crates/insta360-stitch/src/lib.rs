//! This crate stitches Insta360 dual-fisheye photos (`.insp`) and videos (`.insv`) into equirectangular panoramas.
//! It uses the calibration and motion data in the trailer of each file.
//!
//! The crate does not decode or encode images, and it does not run ffmpeg.
//! It reads the trailer from a seekable source and stitches decoded pixels.
//! For videos, [`FrameStitcher`] stitches decoded frames with a stabilization rotation for each frame
//! ([`frame_orientations`]), and [`video_maps`] makes remap planes for an ffmpeg-only pipeline.
//!
//! ```no_run
//! use insta360_stitch::{read_trailer, orientation, LensModel, stitch_photo, Image};
//!
//! let mut file = std::fs::File::open("IMG_20260821_101803_00_013.insp")?;
//! let trailer = read_trailer(&mut file)?;
//! let (width, height, pixels) = (11904, 5952, vec![0u8; 11904 * 5952 * 3]); // The decoded JPEG
//! let model = LensModel::new(&trailer, height, orientation(&trailer)?)?;
//! let stitched = stitch_photo(&Image { width, height, channels: 3, data: &pixels }, &model)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod blend;
mod clock;
mod dis;
mod error;
mod fusion;
mod lens;
pub mod math;
mod mp4;
mod orientation;
mod preview;
mod seamflow;
mod stitch;
mod trailer;
mod varref;
mod video;

pub use clock::{camera_utc_offset_minutes, wall_clock_seconds};
pub use error::{Error, Result};
pub use lens::{blend_weight, Lens, LensModel, Mei, RaySample, RowMaps};
pub use mp4::{check_different_files, write_spherical_mp4, write_spherical_mp4_file};
pub use orientation::{
    box_gravity_quats, butter2_lowpass, filtfilt2, frame_orientations, orientation,
    rolling_rotations, Biquad, GravityQuats, Stabilization,
};
pub use preview::preview_rgb;
pub use stitch::{
    check_photo_layout, stitch_photo, stitch_photo_band, stitch_photo_band_with, stitch_photo_rows,
    video_maps, Image, PhotoSeam, VideoMaps, FFMPEG_FILTER, MAX_VIDEO_WIDTH,
};
pub use trailer::{
    frame_times_us, lens_frame_times_us, parse_trailer, read_imu, read_preview, read_trailer,
    trailer_length, uses_exposure_table, Exposure, Imu, Preview, Trailer, TAIL_LENGTH,
};
pub use video::{FrameFlow, FrameRotation, FrameStitcher};

/// The row rotations of a frame for the rolling shutter correction: the first, the last and three rows between.
/// The rotation changes by less than a degree in one frame, thus 5 rotations are sufficient.
const ROLLING_SAMPLES: usize = 5;

/// A trailer and its level rotation. The constructor calculates the rotation one time.
/// Use one `Stitcher` for all the stitch operations on one file.
pub struct Stitcher {
    pub trailer: Trailer,
    level: std::result::Result<math::Mat3, String>,
    unsupported: Option<String>,
    /// The lens model and the seam of the last photo, for the next band of the same photo.
    photo: std::sync::Mutex<Option<(PhotoKey, PhotoPlan)>>,
}

type PhotoPlan = std::sync::Arc<(LensModel, PhotoSeam)>;

/// The size, the channels and a fingerprint of the pixels of a photo.
/// shortcut: the fingerprint reads one byte in 65536 bytes; two photos that differ only in other bytes share a
/// seam. Upgrade: let the caller keep the seam (`stitch_photo_band_with`).
#[derive(PartialEq, Eq, Clone, Copy)]
struct PhotoKey(usize, usize, usize, u64);

impl PhotoKey {
    fn of(source: &Image) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let step = (source.data.len() / 65536).max(1);
        for b in source.data.iter().step_by(step) {
            b.hash(&mut hasher);
        }
        PhotoKey(
            source.width,
            source.height,
            source.channels,
            hasher.finish(),
        )
    }
}

/// The reason of an error without the "unsupported: " prefix.
fn reason(error: Error) -> String {
    match error {
        Error::Unsupported(reason) => reason,
        other => other.to_string(),
    }
}

impl Stitcher {
    pub fn new(trailer: Trailer) -> Self {
        let level = orientation(&trailer).map_err(reason);
        let unsupported = match &level {
            Ok(level) => LensModel::new(&trailer, 1, *level).err().map(reason),
            Err(why) => Some(why.clone()),
        };
        Stitcher {
            trailer,
            level,
            unsupported,
            photo: std::sync::Mutex::new(None),
        }
    }

    /// The reason why the library cannot stitch the file, or `None`.
    /// The calibration, the lens guard and the IMU data must be supported.
    pub fn unsupported_reason(&self) -> Option<&str> {
        self.unsupported.as_deref()
    }

    /// Makes the lens model for a lens crop or lens stream of `lens_size` pixels.
    pub fn model(&self, lens_size: usize) -> Result<LensModel> {
        match &self.level {
            Ok(level) => LensModel::new(&self.trailer, lens_size, *level),
            Err(why) => Err(Error::Unsupported(why.clone())),
        }
    }

    /// Stitches the full photo, or only the band `(first_row, rows)`, into a new buffer.
    /// It checks all the arguments before the allocation.
    pub fn photo(&self, source: &Image, band: Option<(usize, usize)>) -> Result<Vec<u8>> {
        stitch::check_source(source)?;
        let (first_row, rows) = band.unwrap_or((0, source.height));
        if first_row
            .checked_add(rows)
            .is_none_or(|end| end > source.height)
        {
            return Err(Error::InvalidArgument(format!(
                "rows {first_row}..{first_row}+{rows} are outside the photo"
            )));
        }
        let key = PhotoKey::of(source);
        // The lock stays while the seam is calculated: parallel band calls of one photo then calculate it one
        // time. The `Stitcher` is for one file, thus only calls for that file wait.
        let plan = {
            let mut cache = self.photo.lock().unwrap_or_else(|e| e.into_inner());
            match cache.as_ref().filter(|(k, _)| *k == key) {
                Some((_, plan)) => plan.clone(),
                None => {
                    let model = self.model(source.height)?;
                    let seam = PhotoSeam::new(source, &model)?;
                    let plan = std::sync::Arc::new((model, seam));
                    *cache = Some((key, plan.clone()));
                    plan
                }
            }
        };
        // check_source made sure that width * height * channels does not overflow, and rows <= height.
        let mut out = vec![0; rows * source.width * source.channels];
        stitch_photo_band_with(source, &plan.0, &plan.1, first_row, &mut out)?;
        Ok(out)
    }

    /// Makes the per-frame stitcher for lens streams of `lens_size` x `lens_size` and an output of `width` pixels.
    /// The default width is `2 * lens_size`. See [`FrameStitcher`].
    pub fn frame_stitcher(&self, lens_size: usize, width: Option<usize>) -> Result<FrameStitcher> {
        if let Some(why) = &self.unsupported {
            return Err(Error::Unsupported(why.clone()));
        }
        let model = LensModel::new(&self.trailer, lens_size, math::IDENTITY)?;
        FrameStitcher::new(
            &model,
            lens_size,
            width.unwrap_or(lens_size.saturating_mul(2)),
        )
    }

    /// Gives the rotation of each frame of a video, see [`frame_orientations`] and [`frame_times_us`].
    /// `imu` is the full IMU stream from [`read_imu`].
    pub fn frame_rotations(
        &self,
        imu: &Imu,
        frame_count: usize,
        fps: f64,
        mode: Stabilization,
    ) -> Result<Vec<math::Mat3>> {
        if let Some(why) = &self.unsupported {
            return Err(Error::Unsupported(why.clone()));
        }
        let times = frame_times_us(&self.trailer, frame_count, fps);
        frame_orientations(&self.trailer, imu, &times, mode)
    }

    /// Gives the rotations of each frame of a video with the rows of the rolling shutter, see
    /// [`rolling_rotations`] and [`lens_frame_times_us`]. `rolling` false, or a file without a rolling shutter
    /// time, gives one rotation for all rows. `imu` is the full IMU stream from [`read_imu`].
    pub fn frame_motions(
        &self,
        imu: &Imu,
        frame_count: usize,
        fps: f64,
        mode: Stabilization,
        rolling: bool,
    ) -> Result<Vec<FrameRotation>> {
        let frames = self.frame_rotations(imu, frame_count, fps, mode)?;
        let times = lens_frame_times_us(&self.trailer, frame_count, fps);
        let sweep_us = if rolling {
            self.trailer.rolling_shutter_ms * 1e3
        } else {
            0.0
        };
        let rows = rolling_rotations(
            &self.trailer,
            imu,
            &frames,
            &times,
            sweep_us,
            ROLLING_SAMPLES,
        )?;
        Ok(frames
            .into_iter()
            .zip(rows)
            .map(|(centre, rows)| FrameRotation { centre, rows })
            .collect())
    }

    /// Makes the remap planes for lens streams of `lens_width` x `lens_height`. See [`video_maps`].
    /// The default width is `2 * lens_width`.
    pub fn video_maps(
        &self,
        lens_width: usize,
        lens_height: usize,
        width: Option<usize>,
    ) -> Result<VideoMaps> {
        let width = width.unwrap_or(lens_width.saturating_mul(2));
        video_maps(&self.model(lens_width)?, width, lens_width, lens_height)
    }
}
