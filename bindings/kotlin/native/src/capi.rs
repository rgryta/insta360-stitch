//! The C API, for iOS (Kotlin/Native cinterop). `include/insta360_stitch.h` declares it.
//!
//! Each fallible function returns a status (`I360_OK` or an error code) and writes the error message to `*error`.
//! Free the message with `i360_string_free`. A null pointer or a string that is not UTF-8 is `I360_INVALID_ARGUMENT`.

use std::ffi::{c_char, CStr, CString};
use std::sync::LazyLock;

use crate::{add_spherical_metadata, core, guarded, invalid, Failure, Handle};

pub const I360_OK: i32 = 0;
pub const I360_UNSUPPORTED: i32 = 1;
pub const I360_IO: i32 = 2;
pub const I360_INVALID_ARGUMENT: i32 = 3;
pub const I360_PANIC: i32 = 4;

/// Bytes that Rust allocated. Free them with `i360_buffer_free`.
#[repr(C)]
pub struct I360Buffer {
    pub data: *mut u8,
    pub len: usize,
    pub width: u32,
    pub height: u32,
}

/// The remap planes. Free them with `i360_video_maps_free`.
#[repr(C)]
pub struct I360VideoMaps {
    pub width: u32,
    pub height: u32,
    pub x0: I360Buffer,
    pub y0: I360Buffer,
    pub x1: I360Buffer,
    pub y1: I360Buffer,
    pub mask: I360Buffer,
}

fn buffer(bytes: Vec<u8>, width: u32, height: u32) -> I360Buffer {
    let bytes = Box::into_raw(bytes.into_boxed_slice());
    I360Buffer {
        len: bytes.len(),
        data: bytes.cast(),
        width,
        height,
    }
}

/// Writes the error to `*error` (if `error` is not null) and gives the status.
fn status(result: Result<(), Failure>, error: *mut *mut c_char) -> i32 {
    let (code, message) = match result {
        Ok(()) => return I360_OK,
        Err(Failure::Core(error @ core::Error::Unsupported(_))) => {
            (I360_UNSUPPORTED, error.to_string())
        }
        Err(Failure::Core(error @ core::Error::Io(_))) => (I360_IO, error.to_string()),
        Err(Failure::Core(error @ core::Error::InvalidArgument(_))) => {
            (I360_INVALID_ARGUMENT, error.to_string())
        }
        Err(Failure::Panic) => (I360_PANIC, "internal error: the stitcher panicked".into()),
    };
    if !error.is_null() {
        // SAFETY: the caller gives a valid pointer to a `char *`.
        unsafe { *error = string(message) };
    }
    code
}

fn string(value: impl Into<String>) -> *mut c_char {
    // An interior NUL cannot come from the library. If it does, the caller gets an empty string.
    CString::new(value.into()).unwrap_or_default().into_raw()
}

/// Reads a C string argument.
///
/// # Safety
/// `value` is null or a valid NUL-terminated string that lives during the call.
unsafe fn str_arg<'a>(value: *const c_char, name: &str) -> Result<&'a str, Failure> {
    if value.is_null() {
        return Err(invalid(format!("{name} is null")));
    }
    // SAFETY: as the function contract says.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| invalid(format!("{name} is not UTF-8")))
}

/// # Safety
/// `file` is null or a pointer from `i360_open` that is not freed.
unsafe fn file_arg<'a>(file: *const Handle) -> Result<&'a Handle, Failure> {
    // SAFETY: as the function contract says.
    unsafe { file.as_ref() }.ok_or_else(|| invalid("the file is null"))
}

/// # Safety
/// `path` is a NUL-terminated UTF-8 string. `out` is a valid pointer. `error` is null or a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_open(
    path: *const c_char,
    out: *mut *mut Handle,
    error: *mut *mut c_char,
) -> i32 {
    status(
        guarded(|| {
            if out.is_null() {
                return Err(invalid("out is null"));
            }
            // SAFETY: as the function contract says.
            let handle = Handle::open(unsafe { str_arg(path, "path") }?.to_owned())?;
            // SAFETY: `out` is not null and valid.
            unsafe { *out = Box::into_raw(Box::new(handle)) };
            Ok(())
        }),
        error,
    )
}

/// # Safety
/// `file` is null or a pointer from `i360_open`. Free it one time, after all other calls on it are complete.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_free(file: *mut Handle) {
    if !file.is_null() {
        // SAFETY: as the function contract says.
        drop(unsafe { Box::from_raw(file) });
    }
}

/// Gives a string to free with `i360_string_free`, or null.
///
/// # Safety
/// `file` is a pointer from `i360_open` that is not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_unsupported_reason(file: *const Handle) -> *mut c_char {
    // SAFETY: as the function contract says.
    match unsafe { file_arg(file) } {
        Ok(file) => file
            .unsupported_reason()
            .map_or(std::ptr::null_mut(), string),
        Err(_) => string("the file is null"),
    }
}

/// Gives a string to free with `i360_string_free`, or null.
///
/// # Safety
/// `file` is a pointer from `i360_open` that is not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_local_time(file: *const Handle) -> *mut c_char {
    // SAFETY: as the function contract says.
    unsafe { file_arg(file) }
        .ok()
        .and_then(|file| file.local_time())
        .map_or(std::ptr::null_mut(), string)
}

/// # Safety
/// `file` is a pointer from `i360_open` that is not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_offset_state(file: *const Handle) -> i32 {
    // SAFETY: as the function contract says.
    unsafe { file_arg(file) }.map_or(0, |file| file.offset_state())
}

/// Writes the preview size and gives true, or gives false if the trailer has no preview.
///
/// # Safety
/// `file` is a pointer from `i360_open` that is not freed. `width` and `height` are valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_preview_size(
    file: *const Handle,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    // SAFETY: as the function contract says.
    match unsafe { file_arg(file) }
        .ok()
        .and_then(|file| file.preview_size())
    {
        Some((w, h)) if !width.is_null() && !height.is_null() => {
            // SAFETY: both pointers are not null and valid.
            unsafe { (*width, *height) = (w, h) };
            true
        }
        _ => false,
    }
}

/// Writes the RGB preview into `*out`. Free it with `i360_buffer_free`.
///
/// # Safety
/// `file` is a pointer from `i360_open` that is not freed. `out` is valid. `error` is null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_preview(
    file: *const Handle,
    out: *mut I360Buffer,
    error: *mut *mut c_char,
) -> i32 {
    status(
        guarded(|| {
            if out.is_null() {
                return Err(invalid("out is null"));
            }
            // SAFETY: as the function contract says.
            let (width, height, rgb) = unsafe { file_arg(file) }?.preview()?;
            // SAFETY: `out` is not null and valid.
            unsafe { *out = buffer(rgb, width, height) };
            Ok(())
        }),
        error,
    )
}

/// Stitches the rows `first_row` to `first_row + rows - 1` of the photo `raw` into `out` (`rows * width * channels`
/// bytes). For the full photo (`0, height`), the stitch writes directly into `out`, without a copy.
///
/// # Safety
/// `file` is a pointer from `i360_open` that is not freed. `raw` has `raw_len` bytes and `out` has `out_len` bytes.
/// `error` is null or valid.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn i360_stitch_into(
    file: *const Handle,
    raw: *const u8,
    raw_len: usize,
    width: i32,
    height: i32,
    channels: i32,
    first_row: i32,
    rows: i32,
    out: *mut u8,
    out_len: usize,
    error: *mut *mut c_char,
) -> i32 {
    status(
        guarded(|| {
            // SAFETY: as the function contract says.
            let file = unsafe { file_arg(file) }?;
            if raw.is_null() || out.is_null() {
                return Err(invalid("raw or out is null"));
            }
            // SAFETY: as the function contract says.
            let (raw, out) = unsafe {
                (
                    std::slice::from_raw_parts(raw, raw_len),
                    std::slice::from_raw_parts_mut(out, out_len),
                )
            };
            if first_row == 0 && rows == height && width >= 0 && height >= 0 && channels >= 0 {
                return file.stitch_into(
                    raw,
                    width as usize,
                    height as usize,
                    channels as usize,
                    out,
                );
            }
            let band = file.stitch(raw, width, height, channels, first_row, rows)?;
            if band.len() != out.len() {
                return Err(invalid(format!(
                    "the output has {} bytes, the band has {}",
                    out.len(),
                    band.len()
                )));
            }
            out.copy_from_slice(&band);
            Ok(())
        }),
        error,
    )
}

/// Makes the remap planes into `*out`. `width` 0 is the default width (`2 * lens_width`). Free them with
/// `i360_video_maps_free`.
///
/// # Safety
/// `file` is a pointer from `i360_open` that is not freed. `out` is valid. `error` is null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_video_maps(
    file: *const Handle,
    lens_width: i32,
    lens_height: i32,
    width: i32,
    out: *mut I360VideoMaps,
    error: *mut *mut c_char,
) -> i32 {
    status(
        guarded(|| {
            if out.is_null() {
                return Err(invalid("out is null"));
            }
            // SAFETY: as the function contract says.
            let maps = unsafe { file_arg(file) }?.video_maps(
                lens_width,
                lens_height,
                (width != 0).then_some(width),
            )?;
            let (w, h) = (maps.width as u32, maps.height as u32);
            let [x0, y0, x1, y1] = maps.planes.map(|plane| buffer(plane, w, h));
            // SAFETY: `out` is not null and valid.
            unsafe {
                *out = I360VideoMaps {
                    width: w,
                    height: h,
                    x0,
                    y0,
                    x1,
                    y1,
                    mask: buffer(maps.mask, w, h),
                }
            };
            Ok(())
        }),
        error,
    )
}

/// # Safety
/// `input` and `output` are NUL-terminated UTF-8 strings. `error` is null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_add_spherical_metadata(
    input: *const c_char,
    output: *const c_char,
    error: *mut *mut c_char,
) -> i32 {
    status(
        guarded(|| {
            // SAFETY: as the function contract says.
            let (input, output) = unsafe { (str_arg(input, "input")?, str_arg(output, "output")?) };
            add_spherical_metadata(input, output)
        }),
        error,
    )
}

/// Writes the UTC offset of the camera in minutes and gives true, or gives false if the difference is not a real time
/// zone (or `local_time` is not valid).
///
/// # Safety
/// `local_time` is a NUL-terminated string. `minutes` is valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_camera_utc_offset_minutes(
    local_time: *const c_char,
    utc_unix_seconds: i64,
    minutes: *mut i32,
) -> bool {
    // SAFETY: as the function contract says.
    let Ok(local_time) = (unsafe { str_arg(local_time, "local_time") }) else {
        return false;
    };
    match core::camera_utc_offset_minutes(local_time, utc_unix_seconds) {
        Some(offset) if !minutes.is_null() => {
            // SAFETY: `minutes` is not null and valid.
            unsafe { *minutes = offset };
            true
        }
        _ => false,
    }
}

static FFMPEG_FILTER: LazyLock<CString> =
    LazyLock::new(|| CString::new(core::FFMPEG_FILTER).unwrap_or_default());

/// The ffmpeg `-filter_complex` for the video maps. Do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn i360_ffmpeg_filter() -> *const c_char {
    FFMPEG_FILTER.as_ptr()
}

/// # Safety
/// `value` is null or a string from this library that is not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_string_free(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: as the function contract says.
        drop(unsafe { CString::from_raw(value) });
    }
}

/// # Safety
/// `buffer` comes from this library and is not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_buffer_free(buffer: I360Buffer) {
    if !buffer.data.is_null() {
        // SAFETY: `buffer` comes from `buffer()`, a boxed slice of `len` bytes.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buffer.data, buffer.len)) });
    }
}

/// # Safety
/// `maps` comes from `i360_video_maps` and is not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i360_video_maps_free(maps: I360VideoMaps) {
    // SAFETY: as the function contract says.
    unsafe {
        for plane in [maps.x0, maps.y0, maps.x1, maps.y1, maps.mask] {
            i360_buffer_free(plane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/synthetic_x5.insp"
    );

    fn c(value: &str) -> CString {
        CString::new(value).unwrap()
    }

    /// Opens a file and gives the status, the handle and the error message.
    fn open(path: &str) -> (i32, *mut Handle, Option<String>) {
        let (mut file, mut error) = (std::ptr::null_mut(), std::ptr::null_mut());
        let code = unsafe { i360_open(c(path).as_ptr(), &mut file, &mut error) };
        (code, file, take(error))
    }

    fn take(error: *mut c_char) -> Option<String> {
        (!error.is_null()).then(|| {
            let text = unsafe { CStr::from_ptr(error) }
                .to_str()
                .unwrap()
                .to_owned();
            unsafe { i360_string_free(error) };
            text
        })
    }

    #[test]
    fn opens_and_stitches_through_the_c_api() {
        let (code, file, error) = open(FIXTURE);
        assert_eq!((code, error), (I360_OK, None));
        assert!(unsafe { i360_unsupported_reason(file) }.is_null());
        let pattern: Vec<u8> = (0..64 * 32 * 3).map(|i| (i * 7 % 251) as u8).collect();
        let mut out = vec![0; pattern.len()];
        let mut error = std::ptr::null_mut();
        let code = unsafe {
            i360_stitch_into(
                file,
                pattern.as_ptr(),
                pattern.len(),
                64,
                32,
                3,
                0,
                32,
                out.as_mut_ptr(),
                out.len(),
                &mut error,
            )
        };
        assert_eq!((code, take(error)), (I360_OK, None));
        let expected = unsafe { &*file }
            .stitch(&pattern, 64, 32, 3, 0, 32)
            .ok()
            .unwrap();
        assert_eq!(out, expected);
        // A band gives the same rows.
        let mut band = vec![0; 7 * 64 * 3];
        let code = unsafe {
            i360_stitch_into(
                file,
                pattern.as_ptr(),
                pattern.len(),
                64,
                32,
                3,
                10,
                7,
                band.as_mut_ptr(),
                band.len(),
                &mut error,
            )
        };
        assert_eq!(code, I360_OK);
        assert_eq!(band, expected[10 * 64 * 3..17 * 64 * 3]);
        let mut maps = std::mem::MaybeUninit::<I360VideoMaps>::uninit();
        assert_eq!(
            unsafe { i360_video_maps(file, 64, 64, 0, maps.as_mut_ptr(), &mut error) },
            I360_OK
        );
        let maps = unsafe { maps.assume_init() };
        assert_eq!(
            (maps.width, maps.height, maps.x0.len, maps.mask.len),
            (128, 64, 128 * 64 * 2, 128 * 64)
        );
        unsafe { i360_video_maps_free(maps) };
        let mut preview = std::mem::MaybeUninit::<I360Buffer>::uninit();
        assert_eq!(
            unsafe { i360_preview(file, preview.as_mut_ptr(), &mut error) },
            I360_OK
        );
        let preview = unsafe { preview.assume_init() };
        assert_eq!((preview.width, preview.height, preview.len), (2, 2, 12));
        unsafe { i360_buffer_free(preview) };
        unsafe { i360_free(file) };
    }

    #[test]
    fn maps_each_error_to_its_status() {
        let (code, file, error) = open("/nonexistent/IMG_1.insp");
        assert_eq!(code, I360_IO);
        assert!(file.is_null());
        assert!(error.unwrap().starts_with("i/o error"));
        let (code, _, error) = open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        assert_eq!(code, I360_UNSUPPORTED);
        assert!(error
            .unwrap()
            .starts_with("unsupported: no Insta360 trailer"));
        let (mut out, mut error) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert_eq!(
            unsafe { i360_open(std::ptr::null(), &mut out, &mut error) },
            I360_INVALID_ARGUMENT
        );
        assert_eq!(take(error).unwrap(), "invalid argument: path is null");
        let (_, file, _) = open(FIXTURE);
        let pattern = vec![0u8; 64 * 32 * 3];
        let mut small = vec![0u8; 10];
        let code = unsafe {
            i360_stitch_into(
                file,
                pattern.as_ptr(),
                pattern.len(),
                64,
                32,
                3,
                0,
                32,
                small.as_mut_ptr(),
                small.len(),
                &mut error,
            )
        };
        assert_eq!(code, I360_INVALID_ARGUMENT);
        assert!(take(error)
            .unwrap()
            .starts_with("invalid argument: the output has 10 bytes"));
        let code = unsafe {
            i360_stitch_into(
                file,
                pattern.as_ptr(),
                pattern.len(),
                -1,
                32,
                3,
                0,
                32,
                small.as_mut_ptr(),
                small.len(),
                &mut error,
            )
        };
        assert_eq!(
            (code, take(error).unwrap().as_str()),
            (
                I360_INVALID_ARGUMENT,
                "invalid argument: width must not be negative, it is -1"
            )
        );
        unsafe { i360_free(file) };
    }

    #[test]
    fn module_functions() {
        let filter = unsafe { CStr::from_ptr(i360_ffmpeg_filter()) }
            .to_str()
            .unwrap();
        assert!(filter.contains("maskedmerge"));
        let mut minutes = 0;
        assert!(unsafe {
            i360_camera_utc_offset_minutes(
                c("20260821105307").as_ptr(),
                1_787_280_787,
                &mut minutes,
            )
        });
        assert_eq!(minutes, 480);
        assert!(!unsafe {
            i360_camera_utc_offset_minutes(
                c("20260821050207").as_ptr(),
                1_787_280_787,
                &mut minutes,
            )
        });
        let mut error = std::ptr::null_mut();
        let code = unsafe {
            i360_add_spherical_metadata(
                c("/nonexistent/a.mp4").as_ptr(),
                c("/nonexistent/b.mp4").as_ptr(),
                &mut error,
            )
        };
        assert_eq!(code, I360_IO);
        assert!(take(error).is_some());
    }

    /// The header and this file must agree on the status codes.
    #[test]
    fn header_has_the_same_status_codes() {
        let header = include_str!("../include/insta360_stitch.h");
        for (name, value) in [
            ("I360_OK", I360_OK),
            ("I360_UNSUPPORTED", I360_UNSUPPORTED),
            ("I360_IO", I360_IO),
            ("I360_INVALID_ARGUMENT", I360_INVALID_ARGUMENT),
            ("I360_PANIC", I360_PANIC),
        ] {
            assert!(
                header.contains(&format!("#define {name} {value}\n")),
                "{name} in the header"
            );
        }
    }
}
