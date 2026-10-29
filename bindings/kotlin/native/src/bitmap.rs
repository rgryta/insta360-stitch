//! Android bitmaps (libjnigraphics): the bridge reads and writes their pixels directly, without a copy.

use std::ffi::c_void;

use jni::sys::{jobject, JNIEnv};

use crate::{invalid, Failure};

#[repr(C)]
#[derive(Default)]
struct AndroidBitmapInfo {
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    flags: u32,
}

const ANDROID_BITMAP_RESULT_SUCCESS: i32 = 0;
const ANDROID_BITMAP_FORMAT_RGBA_8888: i32 = 1;
const ANDROID_BITMAP_FLAGS_IS_HARDWARE: u32 = 1 << 31;

#[link(name = "jnigraphics")]
unsafe extern "C" {
    fn AndroidBitmap_getInfo(
        env: *mut JNIEnv,
        bitmap: jobject,
        info: *mut AndroidBitmapInfo,
    ) -> i32;
    fn AndroidBitmap_lockPixels(env: *mut JNIEnv, bitmap: jobject, pixels: *mut *mut c_void)
        -> i32;
    fn AndroidBitmap_unlockPixels(env: *mut JNIEnv, bitmap: jobject) -> i32;
}

/// The locked pixels of an `ARGB_8888` software bitmap, as RGBA bytes. The drop unlocks them, also after an error or
/// a panic.
pub struct LockedBitmap {
    env: *mut JNIEnv,
    bitmap: jobject,
    pixels: *mut u8,
    pub width: usize,
    pub height: usize,
}

impl LockedBitmap {
    /// Locks the pixels of `bitmap`. Another config, a hardware bitmap, a recycled bitmap, or a row stride that is not
    /// `width * 4` is an invalid argument.
    ///
    /// # Safety
    /// `env` and `bitmap` must be valid for the current native method call, and the value must not outlive it.
    pub unsafe fn lock(env: *mut JNIEnv, bitmap: jobject, name: &str) -> Result<Self, Failure> {
        let mut info = AndroidBitmapInfo::default();
        // SAFETY: the caller gives a valid env and bitmap; `info` is a valid output.
        if unsafe { AndroidBitmap_getInfo(env, bitmap, &mut info) } != ANDROID_BITMAP_RESULT_SUCCESS
        {
            return Err(invalid(format!("the {name} bitmap is not valid")));
        }
        if info.format != ANDROID_BITMAP_FORMAT_RGBA_8888
            || info.flags & ANDROID_BITMAP_FLAGS_IS_HARDWARE != 0
        {
            return Err(invalid(format!(
                "the {name} bitmap must be an ARGB_8888 software bitmap"
            )));
        }
        if info.stride != info.width * 4 {
            return Err(invalid(format!(
                "the {name} bitmap has a row stride of {} bytes, not width * 4",
                info.stride
            )));
        }
        let mut pixels = std::ptr::null_mut();
        // SAFETY: as above. A recycled bitmap gives an error here.
        if unsafe { AndroidBitmap_lockPixels(env, bitmap, &mut pixels) }
            != ANDROID_BITMAP_RESULT_SUCCESS
            || pixels.is_null()
        {
            return Err(invalid(format!(
                "the pixels of the {name} bitmap are not available"
            )));
        }
        Ok(LockedBitmap {
            env,
            bitmap,
            pixels: pixels.cast(),
            width: info.width as usize,
            height: info.height as usize,
        })
    }

    fn len(&self) -> usize {
        self.width * self.height * 4
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the pixels stay locked and valid while `self` lives. The size comes from the bitmap.
        unsafe { std::slice::from_raw_parts(self.pixels, self.len()) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `bytes`. `&mut self` gives exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.pixels, self.len()) }
    }
}

impl Drop for LockedBitmap {
    fn drop(&mut self) {
        // SAFETY: `lock` locked the bitmap in the same native method call.
        unsafe { AndroidBitmap_unlockPixels(self.env, self.bitmap) };
    }
}
