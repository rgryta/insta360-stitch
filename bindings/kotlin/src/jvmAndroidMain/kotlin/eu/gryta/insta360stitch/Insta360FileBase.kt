package eu.gryta.insta360stitch

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asExecutor
import kotlinx.coroutines.withContext
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executor

/** The executor of the `...Async` forms without an `Executor` argument. */
internal val defaultExecutor: Executor = Dispatchers.IO.asExecutor()

/**
 * The part of [Insta360File] that the JVM and Android share. Do not use this class directly.
 *
 * An Insta360 .insp or .insv file. The open operation reads its trailer and calculates the level rotation one time.
 * The file holds native memory: close it after use (`use {}` in Kotlin, try-with-resources in Java). If you do not
 * close it, a `Cleaner` frees the memory after the garbage collector finds the file.
 *
 * Java: each slow operation has a blocking form (for example `stitchPhoto`) and a `CompletableFuture` form
 * (`stitchPhotoAsync`). Kotlin: use the `suspend` forms (`open`, `stitchPhoto`, ...); the `...Blocking` and
 * `...Async` forms are for Java.
 */
abstract class Insta360FileBase internal constructor(value: Long) : AutoCloseable {
    internal val handle = NativeHandle(value)
    private val cleanable = cleaner.register(this, handle)

    @get:JvmName("isStitchable")
    val stitchable: Boolean get() = handle.use { Native.unsupportedReason(it) == null }
    val unsupportedReason: String? get() = handle.use { Native.unsupportedReason(it) }
    val localTime: String? get() = handle.use { Native.localTime(it) }
    val offsetState: Int get() = handle.use { Native.offsetState(it) }
    val previewSize: Size? get() = handle.use { Native.previewSize(it) }?.let { Size(it[0], it[1]) }

    @JvmSynthetic
    suspend fun preview(): PreviewImage = withContext(Dispatchers.IO) { previewBlocking() }

    /** Gives the small equirectangular preview that the camera stitched. */
    @JvmName("preview")
    @Throws(IOException::class)
    fun previewBlocking(): PreviewImage = handle.use {
        val size = IntArray(2)
        val rgb = Native.preview(it, size)
        PreviewImage(size[0], size[1], rgb)
    }

    @JvmOverloads
    fun previewAsync(executor: Executor = defaultExecutor): CompletableFuture<PreviewImage> =
        CompletableFuture.supplyAsync({ previewBlocking() }, executor)

    @JvmSynthetic
    suspend fun stitchPhoto(raw: ByteArray, width: Int, height: Int, channels: Int): ByteArray =
        withContext(Dispatchers.IO) { stitchPhotoBlocking(raw, width, height, channels) }

    /** See the `suspend` form. */
    @JvmName("stitchPhoto")
    fun stitchPhotoBlocking(raw: ByteArray, width: Int, height: Int, channels: Int): ByteArray =
        handle.use { Native.stitch(it, raw, width, height, channels, 0, height) }

    @JvmOverloads
    fun stitchPhotoAsync(
        raw: ByteArray,
        width: Int,
        height: Int,
        channels: Int,
        executor: Executor = defaultExecutor,
    ): CompletableFuture<ByteArray> =
        CompletableFuture.supplyAsync({ stitchPhotoBlocking(raw, width, height, channels) }, executor)

    @JvmSynthetic
    suspend fun stitchPhotoBand(
        raw: ByteArray,
        width: Int,
        height: Int,
        channels: Int,
        firstRow: Int,
        rows: Int,
    ): ByteArray = withContext(Dispatchers.IO) { stitchPhotoBandBlocking(raw, width, height, channels, firstRow, rows) }

    /** See the `suspend` form. */
    @JvmName("stitchPhotoBand")
    fun stitchPhotoBandBlocking(raw: ByteArray, width: Int, height: Int, channels: Int, firstRow: Int, rows: Int): ByteArray =
        handle.use { Native.stitch(it, raw, width, height, channels, firstRow, rows) }

    @JvmOverloads
    fun stitchPhotoBandAsync(
        raw: ByteArray,
        width: Int,
        height: Int,
        channels: Int,
        firstRow: Int,
        rows: Int,
        executor: Executor = defaultExecutor,
    ): CompletableFuture<ByteArray> =
        CompletableFuture.supplyAsync({ stitchPhotoBandBlocking(raw, width, height, channels, firstRow, rows) }, executor)

    @JvmSynthetic
    suspend fun videoMaps(lensWidth: Int, lensHeight: Int): VideoMaps = videoMaps(lensWidth, lensHeight, null)

    @JvmSynthetic
    suspend fun videoMaps(lensWidth: Int, lensHeight: Int, width: Int?): VideoMaps =
        withContext(Dispatchers.IO) { videoMapsBlocking(lensWidth, lensHeight, width) }

    /** See the `suspend` form. Java: `videoMaps(lensWidth, lensHeight)` uses the default width. */
    @JvmName("videoMaps")
    @JvmOverloads
    fun videoMapsBlocking(lensWidth: Int, lensHeight: Int, width: Int? = null): VideoMaps {
        // -1 is the default width in the bridge. A negative width from the caller is an error there.
        require(width != -1) { "invalid argument: width must not be negative, it is -1" }
        return handle.use {
            val size = IntArray(2)
            val (x0, y0, x1, y1, mask) = Native.videoMaps(it, lensWidth, lensHeight, width ?: -1, size)
            VideoMaps(size[0], size[1], x0, y0, x1, y1, mask)
        }
    }

    @JvmOverloads
    fun videoMapsAsync(
        lensWidth: Int,
        lensHeight: Int,
        width: Int? = null,
        executor: Executor = defaultExecutor,
    ): CompletableFuture<VideoMaps> =
        CompletableFuture.supplyAsync({ videoMapsBlocking(lensWidth, lensHeight, width) }, executor)

    /** Frees the native memory. It waits for the calls that run. A second `close()` does nothing. */
    override fun close() = cleanable.clean()
}
