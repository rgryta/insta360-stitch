//! The Python bindings. A trailer read reads only the records that it needs.
//! The stitch operations release the GIL.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use ::insta360_stitch as core;
use pyo3::buffer::PyBuffer;
use pyo3::create_exception;
use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

create_exception!(
    insta360_stitch,
    UnsupportedError,
    PyValueError,
    "The library cannot stitch this file. A retry will not help."
);

fn to_py(error: core::Error) -> PyErr {
    match error {
        core::Error::Unsupported(reason) => UnsupportedError::new_err(reason),
        core::Error::Io(error) => PyOSError::new_err(error.to_string()),
        core::Error::InvalidArgument(reason) => PyValueError::new_err(reason),
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

/// Gives the bytes of a C-contiguous `u8` buffer without a copy.
fn bytes_of(raw: &PyBuffer<u8>) -> PyResult<&[u8]> {
    if !raw.is_c_contiguous() {
        return Err(PyValueError::new_err("raw must be a C-contiguous buffer"));
    }
    // SAFETY: The buffer view stays valid while `raw` exists, and the stitcher only reads it.
    Ok(unsafe { std::slice::from_raw_parts(raw.buf_ptr() as *const u8, raw.len_bytes()) })
}

/// An Insta360 .insp or .insv file. The constructor reads its trailer.
#[pyclass(frozen, module = "insta360_stitch")]
struct Insta360File {
    path: PathBuf,
    trailer: core::Trailer,
    unsupported: Option<String>,
    /// The level rotation. The constructor calculates it one time, thus a stitch call does not calculate it again.
    level: std::result::Result<core::math::Mat3, String>,
}

impl Insta360File {
    fn model(&self, lens_size: usize) -> core::Result<core::LensModel> {
        match &self.level {
            Ok(level) => core::LensModel::new(&self.trailer, lens_size, *level),
            Err(reason) => Err(core::Error::Unsupported(reason.clone())),
        }
    }
}

#[pymethods]
impl Insta360File {
    #[new]
    fn new(path: PathBuf) -> PyResult<Self> {
        let file = File::open(&path).map_err(|e| to_py(core::Error::Io(e)))?;
        let trailer = core::read_trailer(&mut BufReader::new(file)).map_err(to_py)?;
        let level = level_of(&trailer);
        let unsupported = unsupported_reason(&trailer, &level);
        Ok(Insta360File {
            path,
            trailer,
            unsupported,
            level,
        })
    }

    /// Is ``True`` if the library supports the calibration, the lens guard and the IMU data.
    #[getter]
    fn stitchable(&self) -> bool {
        self.unsupported.is_none()
    }

    /// The reason why the library cannot stitch the file, or ``None``.
    #[getter]
    fn unsupported_reason(&self) -> Option<String> {
        self.unsupported.clone()
    }

    /// The time on the camera clock when the camera made the file, as ``YYYYMMDDhhmmss``.
    #[getter]
    fn local_time(&self) -> Option<String> {
        self.trailer.local_time.clone()
    }

    /// The state of the lens guard or accessory: 0 = none, 5 = premium guard.
    #[getter]
    fn offset_state(&self) -> u64 {
        self.trailer.offset_state
    }

    /// The width and height of the preview, or ``None`` if the trailer has no preview.
    #[getter]
    fn preview_size(&self) -> Option<(u32, u32)> {
        self.trailer.preview_size()
    }

    /// Gives the small preview that the camera stitched, as ``(width, height, rgb_bytes)``.
    fn preview<'py>(&self, py: Python<'py>) -> PyResult<(u32, u32, Bound<'py, PyBytes>)> {
        let file = File::open(&self.path).map_err(|e| to_py(core::Error::Io(e)))?;
        let preview =
            core::read_preview(&mut BufReader::new(file), &self.trailer).map_err(to_py)?;
        Ok((
            preview.width,
            preview.height,
            PyBytes::new(py, &core::preview_rgb(&preview)),
        ))
    }

    /// Stitches the decoded raw pixels into an equirectangular image.
    /// The two lenses are side by side (``width == 2 * height``). The output has the same size and channels.
    #[pyo3(signature = (raw, width, height, channels = 3))]
    fn stitch_photo<'py>(
        &self,
        py: Python<'py>,
        raw: PyBuffer<u8>,
        width: usize,
        height: usize,
        channels: usize,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let raw = bytes_of(&raw)?;
        let out = py
            .detach(|| {
                let model = self.model(height)?;
                core::stitch_photo(
                    &core::Image {
                        width,
                        height,
                        channels,
                        data: raw,
                    },
                    &model,
                )
            })
            .map_err(to_py)?;
        Ok(PyBytes::new(py, &out))
    }

    /// Stitches only the output rows ``first_row`` to ``first_row + rows - 1`` of the photo.
    /// Use it to stitch a photo in parts, for example to show progress or to limit memory.
    /// The arguments ``raw``, ``width``, ``height`` and ``channels`` are the same as for ``stitch_photo``.
    #[pyo3(signature = (raw, width, height, first_row, rows, channels = 3))]
    #[allow(clippy::too_many_arguments)]
    fn stitch_photo_band<'py>(
        &self,
        py: Python<'py>,
        raw: PyBuffer<u8>,
        width: usize,
        height: usize,
        first_row: usize,
        rows: usize,
        channels: usize,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let raw = bytes_of(&raw)?;
        let out = py
            .detach(|| {
                let model = self.model(height)?;
                let source = core::Image {
                    width,
                    height,
                    channels,
                    data: raw,
                };
                // Check the range before the allocation. A very large `rows` must not stop the process.
                if first_row.saturating_add(rows) > height {
                    return Err(core::Error::InvalidArgument(format!(
                        "rows {first_row}..{} are outside the photo",
                        first_row.saturating_add(rows)
                    )));
                }
                let mut out = vec![0; rows * width * channels];
                core::stitch_photo_band(&source, &model, first_row, &mut out).map(|()| out)
            })
            .map_err(to_py)?;
        Ok(PyBytes::new(py, &out))
    }

    /// Makes the remap planes for a .insv file with lens streams of ``lens_width`` x ``lens_height``.
    /// The output is ``width`` x ``width / 2``. The default width is ``2 * lens_width``, which keeps the resolution of the lenses.
    /// Give the planes to ffmpeg with ``FFMPEG_FILTER``.
    #[pyo3(signature = (lens_width, lens_height, width = None))]
    fn video_maps<'py>(
        &self,
        py: Python<'py>,
        lens_width: usize,
        lens_height: usize,
        width: Option<usize>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let width = width.unwrap_or(2 * lens_width);
        let maps = py
            .detach(|| core::video_maps(&self.model(lens_width)?, width, lens_width, lens_height))
            .map_err(to_py)?;
        let le = |plane: &[u16]| {
            plane
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        let dict = PyDict::new(py);
        dict.set_item("width", maps.width)?;
        dict.set_item("height", maps.height)?;
        for (name, plane) in [
            ("x0", &maps.x[0]),
            ("y0", &maps.y[0]),
            ("x1", &maps.x[1]),
            ("y1", &maps.y[1]),
        ] {
            dict.set_item(name, PyBytes::new(py, &le(plane)))?;
        }
        dict.set_item("mask", PyBytes::new(py, &maps.mask))?;
        Ok(dict)
    }
}

/// Gives the UTC offset of the camera in minutes, from the camera clock and the same time in UTC.
/// It gives ``None`` if the difference is not a real time zone. In that condition, the camera clock is wrong.
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

/// Copies the MP4 file at ``input`` to ``output`` and adds the 360 video metadata (Spherical Video V1 and V2).
/// It also moves the ``moov`` box to the start, the same as ffmpeg ``-movflags +faststart``.
/// Give ffmpeg an output without ``+faststart``, then use this function. ``output`` must be a different file.
#[pyfunction]
fn add_spherical_metadata(py: Python<'_>, input: PathBuf, output: PathBuf) -> PyResult<()> {
    py.detach(|| spherical_mp4(&input, &output)).map_err(to_py)
}

#[pyfunction]
fn camera_utc_offset_minutes(local_time: &str, utc_unix_seconds: i64) -> Option<i32> {
    core::camera_utc_offset_minutes(local_time, utc_unix_seconds)
}

#[pymodule(gil_used = false)]
#[pyo3(name = "insta360_stitch")]
fn python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Insta360File>()?;
    m.add_function(wrap_pyfunction!(camera_utc_offset_minutes, m)?)?;
    m.add_function(wrap_pyfunction!(add_spherical_metadata, m)?)?;
    m.add("FFMPEG_FILTER", core::FFMPEG_FILTER)?;
    m.add("UnsupportedError", m.py().get_type::<UnsupportedError>())?;
    Ok(())
}
