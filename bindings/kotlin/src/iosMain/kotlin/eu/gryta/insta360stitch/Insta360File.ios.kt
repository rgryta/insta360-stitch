package eu.gryta.insta360stitch

import eu.gryta.insta360stitch.cinterop.I360Buffer
import eu.gryta.insta360stitch.cinterop.I360File
import eu.gryta.insta360stitch.cinterop.I360VideoMaps
import eu.gryta.insta360stitch.cinterop.i360_buffer_free
import eu.gryta.insta360stitch.cinterop.i360_free
import eu.gryta.insta360stitch.cinterop.i360_local_time
import eu.gryta.insta360stitch.cinterop.i360_offset_state
import eu.gryta.insta360stitch.cinterop.i360_open
import eu.gryta.insta360stitch.cinterop.i360_preview
import eu.gryta.insta360stitch.cinterop.i360_preview_size
import eu.gryta.insta360stitch.cinterop.i360_stitch_into
import eu.gryta.insta360stitch.cinterop.i360_unsupported_reason
import eu.gryta.insta360stitch.cinterop.i360_video_maps
import eu.gryta.insta360stitch.cinterop.i360_video_maps_free
import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.CPointerVar
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.UIntVar
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.alloc
import kotlinx.cinterop.convert
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.nativeHeap
import kotlinx.cinterop.ptr
import kotlinx.cinterop.readBytes
import kotlinx.cinterop.readValue
import kotlinx.cinterop.reinterpret
import kotlinx.cinterop.usePinned
import kotlinx.cinterop.value
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.IO
import kotlinx.coroutines.withContext
import platform.posix.pthread_rwlock_destroy
import platform.posix.pthread_rwlock_init
import platform.posix.pthread_rwlock_rdlock
import platform.posix.pthread_rwlock_t
import platform.posix.pthread_rwlock_unlock
import platform.posix.pthread_rwlock_wrlock
import kotlin.experimental.ExperimentalNativeApi
import kotlin.native.ref.Cleaner
import kotlin.native.ref.createCleaner

/**
 * The pointer to an open native file. Each call holds the read lock. [free] frees the pointer with the write lock:
 * thus it waits for the calls that run, and a call after it throws `IllegalStateException`.
 */
@OptIn(ExperimentalForeignApi::class)
private class NativeHandle(private var file: CPointer<I360File>?) {
    private val lock = nativeHeap.alloc<pthread_rwlock_t>().also { pthread_rwlock_init(it.ptr, null) }

    fun <T> use(block: (CPointer<I360File>) -> T): T {
        pthread_rwlock_rdlock(lock.ptr)
        try {
            return block(checkNotNull(file) { "the file is closed" })
        } finally {
            pthread_rwlock_unlock(lock.ptr)
        }
    }

    fun free() {
        pthread_rwlock_wrlock(lock.ptr)
        try {
            file?.let { i360_free(it) }
            file = null
        } finally {
            pthread_rwlock_unlock(lock.ptr)
        }
    }

    /** Runs one time, when the `Insta360File` is unreachable: then no call can run. */
    fun release() {
        free()
        pthread_rwlock_destroy(lock.ptr)
        nativeHeap.free(lock.rawPtr)
    }
}

/** Pins [this] for [block]. An empty array gives a null pointer: the C API accepts it with the length 0. */
@OptIn(ExperimentalForeignApi::class)
private inline fun <T> ByteArray.pinned(block: (CPointer<ByteVar>?) -> T): T =
    if (isEmpty()) block(null) else usePinned { block(it.addressOf(0)) }

@OptIn(ExperimentalForeignApi::class)
private fun I360Buffer.bytes(): ByteArray = data?.reinterpret<ByteVar>()?.readBytes(len.toInt()) ?: ByteArray(0)

@OptIn(ExperimentalForeignApi::class, ExperimentalNativeApi::class)
actual class Insta360File private constructor(file: CPointer<I360File>) : AutoCloseable {
    private val handle = NativeHandle(file)

    @Suppress("unused") // Frees the native file when the caller does not close it.
    private val cleaner: Cleaner = createCleaner(handle) { it.release() }

    actual val stitchable: Boolean get() = unsupportedReason == null
    actual val unsupportedReason: String? get() = handle.use { i360_unsupported_reason(it).takeString() }
    actual val localTime: String? get() = handle.use { i360_local_time(it).takeString() }
    actual val offsetState: Int get() = handle.use { i360_offset_state(it) }
    actual val previewSize: Size?
        get() = handle.use { file ->
            memScoped {
                val (width, height) = alloc<UIntVar>() to alloc<UIntVar>()
                if (i360_preview_size(file, width.ptr, height.ptr)) Size(width.value.toInt(), height.value.toInt()) else null
            }
        }

    actual suspend fun preview(): PreviewImage = withContext(Dispatchers.IO) {
        handle.use { file ->
            checked { error ->
                val out = alloc<I360Buffer>()
                val status = i360_preview(file, out.ptr, error)
                status to if (status == 0) {
                    PreviewImage(out.width.toInt(), out.height.toInt(), out.bytes()).also { i360_buffer_free(out.readValue()) }
                } else {
                    null
                }
            }!!
        }
    }

    actual suspend fun stitchPhoto(raw: ByteArray, width: Int, height: Int, channels: Int): ByteArray =
        stitchPhotoBand(raw, width, height, channels, 0, height)

    actual suspend fun stitchPhotoBand(
        raw: ByteArray,
        width: Int,
        height: Int,
        channels: Int,
        firstRow: Int,
        rows: Int,
    ): ByteArray = withContext(Dispatchers.IO) {
        // A size that is negative or too large for an array gives an empty output: the bridge then reports the error.
        val size = rows.toLong() * width * channels
        val out = ByteArray(if (size in 0..Int.MAX_VALUE) size.toInt() else 0)
        handle.use { file ->
            checked { error ->
                raw.pinned { input ->
                    out.pinned { output ->
                        val status = i360_stitch_into(
                            file, input?.reinterpret(), raw.size.convert(), width, height, channels, firstRow, rows,
                            output?.reinterpret(), out.size.convert(), error,
                        )
                        status to out
                    }
                }
            }
        }
    }

    actual suspend fun videoMaps(lensWidth: Int, lensHeight: Int): VideoMaps = videoMaps(lensWidth, lensHeight, null)

    actual suspend fun videoMaps(lensWidth: Int, lensHeight: Int, width: Int?): VideoMaps = withContext(Dispatchers.IO) {
        // -1 is the default width in the bridge. A negative width from the caller is an error there.
        require(width != -1) { "invalid argument: width must not be negative, it is -1" }
        handle.use { file ->
            checked { error ->
                val out = alloc<I360VideoMaps>()
                val status = i360_video_maps(file, lensWidth, lensHeight, width ?: -1, out.ptr, error)
                status to if (status == 0) {
                    VideoMaps(
                        out.width.toInt(), out.height.toInt(),
                        out.x0.bytes(), out.y0.bytes(), out.x1.bytes(), out.y1.bytes(), out.mask.bytes(),
                    ).also { i360_video_maps_free(out.readValue()) }
                } else {
                    null
                }
            }!!
        }
    }

    actual override fun close() = handle.free()

    actual companion object {
        actual suspend fun open(path: String): Insta360File = withContext(Dispatchers.IO) {
            checked { error ->
                val out = alloc<CPointerVar<I360File>>()
                val status = i360_open(path, out.ptr, error)
                status to out.value?.let { Insta360File(it) }
            }!!
        }
    }
}
