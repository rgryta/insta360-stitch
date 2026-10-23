//! Python bindings. Trailer reads touch only the records they need; stitching releases the GIL.

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
    "The file cannot be stitched; retrying won't help."
);

fn to_py(error: core::Error) -> PyErr {
    match error {
        core::Error::Unsupported(reason) => UnsupportedError::new_err(reason),
        core::Error::Io(error) => PyOSError::new_err(error.to_string()),
    }
}

/// An Insta360 .insp or .insv file, opened by reading its trailer.
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

    /// Whether the calibration, lens guard and levelling data are all supported.
    #[getter]
    fn stitchable(&self) -> bool {
        self.unsupported.is_none()
    }

    #[getter]
    fn unsupported_reason(&self) -> Option<String> {
        self.unsupported.clone()
    }

    /// The camera's wall clock at creation, ``YYYYMMDDhhmmss``.
    #[getter]
    fn local_time(&self) -> Option<String> {
        self.trailer.local_time.clone()
    }

    /// Lens guard / accessory state (0 = none, 5 = premium guard, ...).
    #[getter]
    fn offset_state(&self) -> u64 {
        self.trailer.offset_state
    }

    #[getter]
    fn preview_size(&self) -> Option<(u32, u32)> {
        self.trailer.preview_size()
    }

    /// The camera's own small stitched preview as ``(width, height, rgb_bytes)``.
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

    /// Stitches decoded raw pixels (both lenses side by side, ``width == 2 * height``) into an
    /// equirectangular image of the same size and channels.
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
            .allow_threads(|| {
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

    /// Remap planes for a .insv whose lens streams are ``width`` x ``lens_height``; feed them
    /// to ffmpeg with ``FFMPEG_FILTER``.
    fn video_maps<'py>(
        &self,
        py: Python<'py>,
        width: usize,
        lens_height: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        let trailer = &self.trailer;
        let maps = py
            .allow_threads(|| {
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

/// The camera's UTC offset in minutes from its wall clock and the same moment in UTC, or
/// ``None`` when the difference isn't a real time zone (a drifted clock).
#[pyfunction]
fn camera_utc_offset_minutes(local_time: &str, utc_unix_seconds: i64) -> Option<i32> {
    core::camera_utc_offset_minutes(local_time, utc_unix_seconds)
}

#[pymodule]
#[pyo3(name = "insta360_stitch")]
fn python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Insta360File>()?;
    m.add_function(wrap_pyfunction!(camera_utc_offset_minutes, m)?)?;
    m.add("FFMPEG_FILTER", core::FFMPEG_FILTER)?;
    m.add("UnsupportedError", m.py().get_type::<UnsupportedError>())?;
    Ok(())
}
