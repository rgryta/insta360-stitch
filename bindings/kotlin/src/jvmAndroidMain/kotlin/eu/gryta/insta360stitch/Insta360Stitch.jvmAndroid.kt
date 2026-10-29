@file:JvmName("Insta360Stitch")

package eu.gryta.insta360stitch

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executor

@JvmSynthetic
actual suspend fun addSphericalMetadata(input: String, output: String) =
    withContext(Dispatchers.IO) { addSphericalMetadataBlocking(input, output) }

/** See the `suspend` form. */
@JvmName("addSphericalMetadata")
@Throws(IOException::class)
fun addSphericalMetadataBlocking(input: String, output: String) = Native.addSphericalMetadata(input, output)

@JvmOverloads
fun addSphericalMetadataAsync(input: String, output: String, executor: Executor = defaultExecutor): CompletableFuture<Void?> =
    CompletableFuture.runAsync({ addSphericalMetadataBlocking(input, output) }, executor)

actual fun cameraUtcOffsetMinutes(localTime: String, utcUnixSeconds: Long): Int? =
    Native.cameraUtcOffsetMinutes(localTime, utcUnixSeconds).takeIf { it != Int.MIN_VALUE }

@JvmField
actual val FFMPEG_FILTER: String = Native.ffmpegFilter()
