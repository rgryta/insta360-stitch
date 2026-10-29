@file:JvmName("Insta360Stitch")

package eu.gryta.insta360stitch

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

actual suspend fun addSphericalMetadata(input: String, output: String) =
    withContext(Dispatchers.IO) { Native.addSphericalMetadata(input, output) }

actual fun cameraUtcOffsetMinutes(localTime: String, utcUnixSeconds: Long): Int? =
    Native.cameraUtcOffsetMinutes(localTime, utcUnixSeconds).takeIf { it != Int.MIN_VALUE }

@JvmField
actual val FFMPEG_FILTER: String = Native.ffmpegFilter()
