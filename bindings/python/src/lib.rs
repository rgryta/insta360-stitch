//! The Python bindings. A trailer read reads only the records that it needs.
//! The stitch operations release the GIL.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use ::insta360_stitch as core;
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
    }
}

/// An Insta360 .insp or .insv file. The constructor reads its trailer.
#[pyclass(frozen, module = "insta360_stitch")]
struct Insta360File {
    path: PathBuf,
    trailer: core::Trailer,
    unsupported: Option<String>,
}

#[pymethods]
impl Insta360File {
    #[new]
    fn new(path: PathBuf) -> PyResult<Self> {
        let file = File::open(&path).map_err(|e| to_py(core::Error::Io(e)))?;
        let trailer = core::read_trailer(&mut BufReader::new(file)).map_err(to_py)?;
        let unsupported = match core::check_stitchable(&trailer) {
            Ok(()) => None,
            Err(core::Error::Unsupported(reason)) => Some(reason),
            Err(error) => return Err(to_py(error)),
        };
        Ok(Insta360File {
            path,
            trailer,
            unsupported,
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
        raw: &[u8],
        width: usize,
        height: usize,
        channels: usize,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let trailer = &self.trailer;
        let out = py
            .detach(|| {
                let model = core::LensModel::new(trailer, height, core::orientation(trailer)?)?;
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

    /// Makes the remap planes for a .insv file with lens streams of ``width`` x ``lens_height``.
    /// Give the planes to ffmpeg with ``FFMPEG_FILTER``.
    fn video_maps<'py>(
        &self,
        py: Python<'py>,
        width: usize,
        lens_height: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        let trailer = &self.trailer;
        let maps = py
            .detach(|| {
                let model = core::LensModel::new(trailer, width, core::orientation(trailer)?)?;
                core::video_maps(&model, width, lens_height)
            })
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
#[pyfunction]
fn camera_utc_offset_minutes(local_time: &str, utc_unix_seconds: i64) -> Option<i32> {
    core::camera_utc_offset_minutes(local_time, utc_unix_seconds)
}

#[pymodule(gil_used = false)]
#[pyo3(name = "insta360_stitch")]
fn python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Insta360File>()?;
    m.add_function(wrap_pyfunction!(camera_utc_offset_minutes, m)?)?;
    m.add("FFMPEG_FILTER", core::FFMPEG_FILTER)?;
    m.add("UnsupportedError", m.py().get_type::<UnsupportedError>())?;
    Ok(())
}
