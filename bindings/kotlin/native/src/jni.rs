//! The JNI functions of `eu.gryta.insta360stitch.Native`, for Android and the JVM.
//! Each function maps a failure to a Java exception, the same way: see `ThrowJava`.

use jni::errors::ErrorPolicy;
use jni::objects::{JByteArray, JClass, JIntArray, JObject, JObjectArray, JString};
use jni::strings::JNIString;
use jni::sys::{jint, jlong};
use jni::{jni_str, Env, EnvUnowned};

use crate::{add_spherical_metadata, core, guarded, Failure, Handle};

/// A failure in a JNI function: from the bridge, or from a JNI call.
enum JniFailure {
    Bridge(Failure),
    Jni(jni::errors::Error),
}

impl From<jni::errors::Error> for JniFailure {
    fn from(error: jni::errors::Error) -> Self {
        JniFailure::Jni(error)
    }
}

impl From<Failure> for JniFailure {
    fn from(failure: Failure) -> Self {
        JniFailure::Bridge(failure)
    }
}

type JniResult<T> = Result<T, JniFailure>;

/// Throws the Java exception for a failure, then returns the default value (0 or null) to Java.
/// - unsupported: `Insta360UnsupportedException` (the reason), i/o: `IOException`, invalid argument:
///   `IllegalArgumentException`, panic: `IllegalStateException`.
/// - The messages are the same as in the Node.js package.
struct ThrowJava;

fn throw(env: &mut Env, class: &jni::strings::JNIStr, message: String) -> jni::errors::Result<()> {
    if !env.exception_check() {
        // throw_new gives Err(JavaException) after it throws. The exception then goes to Java, as necessary.
        let _ = env.throw_new(class, JNIString::from(message));
    }
    Ok(())
}

impl<T: Default> ErrorPolicy<T, JniFailure> for ThrowJava {
    type Captures<'unowned_env_local: 'native_method, 'native_method> = ();

    fn on_error<'unowned_env_local: 'native_method, 'native_method>(
        env: &mut Env<'unowned_env_local>,
        _cap: &mut (),
        failure: JniFailure,
    ) -> jni::errors::Result<T> {
        match failure {
            JniFailure::Bridge(Failure::Core(core::Error::Unsupported(reason))) => throw(
                env,
                jni_str!("eu/gryta/insta360stitch/Insta360UnsupportedException"),
                reason,
            )?,
            JniFailure::Bridge(Failure::Core(error @ core::Error::Io(_))) => {
                throw(env, jni_str!("java/io/IOException"), error.to_string())?
            }
            JniFailure::Bridge(Failure::Core(core::Error::InvalidArgument(reason))) => throw(
                env,
                jni_str!("java/lang/IllegalArgumentException"),
                format!("invalid argument: {reason}"),
            )?,
            JniFailure::Bridge(Failure::Panic) => panic_exception(env)?,
            // A JNI error has a pending Java exception, or it is an error in the bridge.
            JniFailure::Jni(error) => throw(
                env,
                jni_str!("java/lang/IllegalStateException"),
                format!("internal error: {error}"),
            )?,
        }
        Ok(T::default())
    }

    fn on_panic<'unowned_env_local: 'native_method, 'native_method>(
        env: &mut Env<'unowned_env_local>,
        _cap: &mut (),
        _payload: Box<dyn std::any::Any + Send + 'static>,
    ) -> jni::errors::Result<T> {
        panic_exception(env)?;
        Ok(T::default())
    }
}

fn panic_exception(env: &mut Env) -> jni::errors::Result<()> {
    throw(
        env,
        jni_str!("java/lang/IllegalStateException"),
        "internal error: the stitcher panicked".into(),
    )
}

/// Gives the handle that Kotlin keeps. Kotlin guarantees that it is not freed during the call.
fn handle<'a>(handle: jlong) -> &'a Handle {
    // SAFETY: `handle` comes from `open`, and Kotlin frees it only after all calls on it are complete.
    unsafe { &*(handle as *const Handle) }
}

fn optional_string<'local>(
    env: &mut Env<'local>,
    value: Option<&str>,
) -> JniResult<JString<'local>> {
    Ok(match value {
        Some(value) => env.new_string(value)?,
        None => JString::default(),
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_open<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jlong {
    env.with_env(|env| -> JniResult<jlong> {
        let path = path.try_to_string(env)?;
        let handle = guarded(|| Handle::open(path))?;
        Ok(Box::into_raw(Box::new(handle)) as jlong)
    })
    .resolve::<ThrowJava>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_free<'local>(
    _env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: `handle` comes from `open`, and Kotlin calls `free` one time, after all other calls.
        drop(unsafe { Box::from_raw(handle as *mut Handle) });
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_unsupportedReason<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    file: jlong,
) -> JString<'local> {
    env.with_env(|env| optional_string(env, handle(file).unsupported_reason()))
        .resolve::<ThrowJava>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_localTime<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    file: jlong,
) -> JString<'local> {
    env.with_env(|env| optional_string(env, handle(file).local_time()))
        .resolve::<ThrowJava>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_offsetState<'local>(
    _env: EnvUnowned<'local>,
    _class: JClass<'local>,
    file: jlong,
) -> jint {
    handle(file).offset_state()
}

/// Gives `[width, height]`, or null if the trailer has no preview.
#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_previewSize<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    file: jlong,
) -> JIntArray<'local> {
    env.with_env(|env| -> JniResult<_> {
        Ok(match handle(file).preview_size() {
            Some((width, height)) => {
                let array = env.new_int_array(2)?;
                array.set_region(env, 0, &[width as jint, height as jint])?;
                array
            }
            None => Default::default(),
        })
    })
    .resolve::<ThrowJava>()
}

/// Gives the RGB data of the preview, and writes `[width, height]` into `size`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_preview<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    file: jlong,
    size: JIntArray<'local>,
) -> JByteArray<'local> {
    env.with_env(|env| -> JniResult<_> {
        let (width, height, rgb) = guarded(|| handle(file).preview())?;
        size.set_region(env, 0, &[width as jint, height as jint])?;
        Ok(env.byte_array_from_slice(&rgb)?)
    })
    .resolve::<ThrowJava>()
}

/// Stitches the rows `firstRow` to `firstRow + rows - 1` of the photo. The full photo is `0, height`.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_stitch<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    file: jlong,
    raw: JByteArray<'local>,
    width: jint,
    height: jint,
    channels: jint,
    first_row: jint,
    rows: jint,
) -> JByteArray<'local> {
    env.with_env(|env| -> JniResult<_> {
        // One copy into Rust. A critical section would stop the garbage collector during the stitch.
        let raw = env.convert_byte_array(&raw)?;
        let out = guarded(|| handle(file).stitch(&raw, width, height, channels, first_row, rows))?;
        Ok(env.byte_array_from_slice(&out)?)
    })
    .resolve::<ThrowJava>()
}

/// Gives the planes x0, y0, x1, y1 and the mask, and writes `[width, height]` into `size`.
/// `width` is -1 for the default width (`2 * lensWidth`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_videoMaps<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    file: jlong,
    lens_width: jint,
    lens_height: jint,
    width: jint,
    size: JIntArray<'local>,
) -> JObjectArray<'local> {
    env.with_env(|env| -> JniResult<_> {
        let width = (width != -1).then_some(width);
        let maps = guarded(|| handle(file).video_maps(lens_width, lens_height, width))?;
        size.set_region(env, 0, &[maps.width as jint, maps.height as jint])?;
        let planes = env.new_object_array(5, jni_str!("[B"), JObject::null())?;
        for (i, plane) in maps.planes.iter().chain([&maps.mask]).enumerate() {
            let array = env.byte_array_from_slice(plane)?;
            planes.set_element(env, i, &array)?;
        }
        Ok(planes)
    })
    .resolve::<ThrowJava>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_addSphericalMetadata<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    input: JString<'local>,
    output: JString<'local>,
) {
    env.with_env(|env| -> JniResult<()> {
        let (input, output) = (input.try_to_string(env)?, output.try_to_string(env)?);
        Ok(guarded(|| add_spherical_metadata(&input, &output))?)
    })
    .resolve::<ThrowJava>()
}

/// Gives the UTC offset of the camera in minutes, or `Int.MIN_VALUE` if the difference is not a real time zone.
#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_cameraUtcOffsetMinutes<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    local_time: JString<'local>,
    utc_unix_seconds: jlong,
) -> jint {
    env.with_env(|env| -> JniResult<jint> {
        let local_time = local_time.try_to_string(env)?;
        Ok(core::camera_utc_offset_minutes(&local_time, utc_unix_seconds).unwrap_or(jint::MIN))
    })
    .resolve::<ThrowJava>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_eu_gryta_insta360stitch_Native_ffmpegFilter<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    env.with_env(|env| -> JniResult<_> { Ok(env.new_string(core::FFMPEG_FILTER)?) })
        .resolve::<ThrowJava>()
}
