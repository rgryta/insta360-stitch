package eu.gryta.insta360stitch

import eu.gryta.insta360stitch.cinterop.i360_add_spherical_metadata
import eu.gryta.insta360stitch.cinterop.i360_camera_utc_offset_minutes
import eu.gryta.insta360stitch.cinterop.i360_ffmpeg_filter
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.IntVar
import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.toKString
import kotlinx.cinterop.value
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.IO
import kotlinx.coroutines.withContext

@OptIn(ExperimentalForeignApi::class)
actual suspend fun addSphericalMetadata(input: String, output: String): Unit = withContext(Dispatchers.IO) {
    checked { error -> i360_add_spherical_metadata(input, output, error) to Unit }
}

@OptIn(ExperimentalForeignApi::class)
actual fun cameraUtcOffsetMinutes(localTime: String, utcUnixSeconds: Long): Int? = memScoped {
    val minutes = alloc<IntVar>()
    if (i360_camera_utc_offset_minutes(localTime, utcUnixSeconds, minutes.ptr)) minutes.value else null
}

@OptIn(ExperimentalForeignApi::class)
actual val FFMPEG_FILTER: String = i360_ffmpeg_filter()!!.toKString()
