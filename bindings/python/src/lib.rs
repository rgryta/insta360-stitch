//! The Python bindings. A trailer read reads only the records that it needs.
//! Each heavy operation has a sync form, which releases the GIL, and an async form.
//! The async form is a native coroutine: it runs the work on a different thread, thus the event loop does not wait.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

use ::insta360_stitch as core;
use pyo3::buffer::PyBuffer;
use pyo3::create_exception;
use pyo3::exceptions::{PyOSError, PyRuntimeError, PyValueError};
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

/// Refuses a buffer that is not C-contiguous. The stitcher reads the buffer without a copy.
fn check_contiguous(raw: &PyBuffer<u8>) -> PyResult<()> {
    if !raw.is_c_contiguous() {
        return Err(PyValueError::new_err("raw must be a C-contiguous buffer"));
    }
    Ok(())
}

/// Gives the bytes of a C-contiguous `u8` buffer without a copy. Use `check_contiguous` first.
fn bytes_of(raw: &PyBuffer<u8>) -> &[u8] {
    // SAFETY: The buffer view stays valid while `raw` exists, and the stitcher only reads it.
    unsafe { std::slice::from_raw_parts(raw.buf_ptr() as *const u8, raw.len_bytes()) }
}

/// Stitches the full photo, or only the band `(first_row, rows)`.
fn photo(
    stitcher: &core::Stitcher,
    raw: &[u8],
    width: usize,
    height: usize,
    channels: usize,
    band: Option<(usize, usize)>,
) -> core::Result<Vec<u8>> {
    let source = core::Image {
        width,
        height,
        channels,
        data: raw,
    };
    stitcher.photo(&source, band)
}

/// The width, the height, the x0, y0, x1 and y1 planes as little-endian bytes, and the mask.
type MapBytes = (usize, usize, [Vec<u8>; 4], Vec<u8>);

/// Makes the remap planes and converts them to bytes. Thus, only the copy into ``bytes`` needs the GIL.
fn map_bytes(
    stitcher: &core::Stitcher,
    lens_width: usize,
    lens_height: usize,
    width: Option<usize>,
) -> core::Result<MapBytes> {
    let maps = stitcher.video_maps(lens_width, lens_height, width)?;
    Ok((maps.width, maps.height, maps.le_bytes(), maps.mask))
}

fn maps_dict<'py>(py: Python<'py>, maps: &MapBytes) -> PyResult<Bound<'py, PyDict>> {
    let (width, height, planes, mask) = maps;
    let dict = PyDict::new(py);
    dict.set_item("width", width)?;
    dict.set_item("height", height)?;
    for (name, plane) in ["x0", "y0", "x1", "y1"].into_iter().zip(planes) {
        dict.set_item(name, PyBytes::new(py, plane))?;
    }
    dict.set_item("mask", PyBytes::new(py, mask))?;
    Ok(dict)
}

/// Runs `work` on a new thread and waits for its result without the event loop.
/// When the result is ready, the waker continues the coroutine on the event loop.
async fn on_worker<T, F>(work: F) -> PyResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> core::Result<T> + Send + 'static,
{
    let (sender, receiver) = futures_channel::oneshot::channel();
    // A cancelled coroutine does not stop the thread. The work continues to the end, and nothing receives the result.
    std::thread::Builder::new()
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            let _ = sender.send(result);
        })
        .map_err(|error| PyOSError::new_err(format!("cannot start a worker thread: {error}")))?;
    match receiver.await {
        Ok(Ok(result)) => result.map_err(to_py),
        Ok(Err(_)) => Err(PyRuntimeError::new_err(
            "internal error: the stitcher panicked",
        )),
        Err(_) => Err(PyRuntimeError::new_err(
            "internal error: the worker thread stopped",
        )),
    }
}

/// An Insta360 .insp or .insv file. The constructor reads its trailer.
#[pyclass(frozen, module = "insta360_stitch")]
struct Insta360File {
    path: PathBuf,
    /// The trailer and its level rotation. The async operations share it with their worker thread.
    stitcher: Arc<core::Stitcher>,
}

#[pymethods]
impl Insta360File {
    #[new]
    fn new(path: PathBuf) -> PyResult<Self> {
        let file = File::open(&path).map_err(|e| to_py(core::Error::Io(e)))?;
        let trailer = core::read_trailer(&mut BufReader::new(file)).map_err(to_py)?;
        Ok(Insta360File {
            path,
            stitcher: Arc::new(core::Stitcher::new(trailer)),
        })
    }

    /// Is ``True`` if the library supports the calibration, the lens guard and the IMU data.
    #[getter]
    fn stitchable(&self) -> bool {
        self.stitcher.unsupported_reason().is_none()
    }

    /// The reason why the library cannot stitch the file, or ``None``.
    #[getter]
    fn unsupported_reason(&self) -> Option<String> {
        self.stitcher.unsupported_reason().map(String::from)
    }

    /// The time on the camera clock when the camera made the file, as ``YYYYMMDDhhmmss``.
    #[getter]
    fn local_time(&self) -> Option<String> {
        self.stitcher.trailer.local_time.clone()
    }

    /// The state of the lens guard or accessory: 0 = none, 5 = premium guard.
    #[getter]
    fn offset_state(&self) -> u64 {
        self.stitcher.trailer.offset_state
    }

    /// The width and height of the preview, or ``None`` if the trailer has no preview.
    #[getter]
    fn preview_size(&self) -> Option<(u32, u32)> {
        self.stitcher.trailer.preview_size()
    }

    /// Gives the small preview that the camera stitched, as ``(width, height, rgb_bytes)``.
    fn preview<'py>(&self, py: Python<'py>) -> PyResult<(u32, u32, Bound<'py, PyBytes>)> {
        let file = File::open(&self.path).map_err(|e| to_py(core::Error::Io(e)))?;
        let preview =
            core::read_preview(&mut BufReader::new(file), &self.stitcher.trailer).map_err(to_py)?;
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
        check_contiguous(&raw)?;
        let raw = bytes_of(&raw);
        let out = py
            .detach(|| photo(&self.stitcher, raw, width, height, channels, None))
            .map_err(to_py)?;
        Ok(PyBytes::new(py, &out))
    }

    /// The async form of ``stitch_photo``. The event loop does not wait while the photo is stitched.
    #[pyo3(signature = (raw, width, height, channels = 3))]
    async fn stitch_photo_async(
        &self,
        raw: PyBuffer<u8>,
        width: usize,
        height: usize,
        channels: usize,
    ) -> PyResult<Py<PyBytes>> {
        check_contiguous(&raw)?;
        let stitcher = Arc::clone(&self.stitcher);
        let out =
            on_worker(move || photo(&stitcher, bytes_of(&raw), width, height, channels, None))
                .await?;
        Ok(Python::attach(|py| PyBytes::new(py, &out).unbind()))
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
        check_contiguous(&raw)?;
        let raw = bytes_of(&raw);
        let band = Some((first_row, rows));
        let out = py
            .detach(|| photo(&self.stitcher, raw, width, height, channels, band))
            .map_err(to_py)?;
        Ok(PyBytes::new(py, &out))
    }

    /// The async form of ``stitch_photo_band``.
    #[pyo3(signature = (raw, width, height, first_row, rows, channels = 3))]
    #[allow(clippy::too_many_arguments)]
    async fn stitch_photo_band_async(
        &self,
        raw: PyBuffer<u8>,
        width: usize,
        height: usize,
        first_row: usize,
        rows: usize,
        channels: usize,
    ) -> PyResult<Py<PyBytes>> {
        check_contiguous(&raw)?;
        let stitcher = Arc::clone(&self.stitcher);
        let band = Some((first_row, rows));
        let out =
            on_worker(move || photo(&stitcher, bytes_of(&raw), width, height, channels, band))
                .await?;
        Ok(Python::attach(|py| PyBytes::new(py, &out).unbind()))
    }

    /// Makes the remap planes for a .insv file with lens streams of ``lens_width`` x ``lens_height``.
    /// The output is ``width`` x ``width / 2``. The default width is ``2 * lens_width``, which keeps the resolution of the lenses.
    /// ``width`` must be a multiple of 4 and at most 16384.
    /// Give the planes to ffmpeg with ``FFMPEG_FILTER``.
    #[pyo3(signature = (lens_width, lens_height, width = None))]
    fn video_maps<'py>(
        &self,
        py: Python<'py>,
        lens_width: usize,
        lens_height: usize,
        width: Option<usize>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let maps = py
            .detach(|| map_bytes(&self.stitcher, lens_width, lens_height, width))
            .map_err(to_py)?;
        maps_dict(py, &maps)
    }

    /// The async form of ``video_maps``.
    #[pyo3(signature = (lens_width, lens_height, width = None))]
    async fn video_maps_async(
        &self,
        lens_width: usize,
        lens_height: usize,
        width: Option<usize>,
    ) -> PyResult<Py<PyDict>> {
        let stitcher = Arc::clone(&self.stitcher);
        let maps = on_worker(move || map_bytes(&stitcher, lens_width, lens_height, width)).await?;
        Python::attach(|py| Ok(maps_dict(py, &maps)?.unbind()))
    }
}

/// Copies the MP4 file at ``input`` to ``output`` and adds the 360 video metadata (Spherical Video V1 and V2).
/// It also moves the ``moov`` box to the start, the same as ffmpeg ``-movflags +faststart``.
/// Give ffmpeg an output without ``+faststart``, then use this function. ``output`` must be a different file.
/// The function writes a temporary file next to ``output`` and renames it. Thus, after an error, ``output`` did not change.
/// It refuses a fragmented MP4 file.
#[pyfunction]
fn add_spherical_metadata(py: Python<'_>, input: PathBuf, output: PathBuf) -> PyResult<()> {
    py.detach(|| core::write_spherical_mp4_file(&input, &output))
        .map_err(to_py)
}

/// The async form of ``add_spherical_metadata``. The event loop does not wait while the file is copied.
#[pyfunction]
async fn add_spherical_metadata_async(input: PathBuf, output: PathBuf) -> PyResult<()> {
    on_worker(move || core::write_spherical_mp4_file(&input, &output)).await
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
    m.add_function(wrap_pyfunction!(add_spherical_metadata, m)?)?;
    m.add_function(wrap_pyfunction!(add_spherical_metadata_async, m)?)?;
    m.add("FFMPEG_FILTER", core::FFMPEG_FILTER)?;
    m.add("UnsupportedError", m.py().get_type::<UnsupportedError>())?;
    Ok(())
}
