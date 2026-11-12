package eu.gryta.insta360stitch

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The block denoise of a stitched video: the temporal filter of Insta360 Studio. Push the frames in order and take
 * the filtered frames in the same order.
 *
 * A frame is planar YUV 4:2:0 with 8 bit samples: `width x height` bytes of Y, then U, then V ([frameBytes] bytes).
 * Each frame is averaged with the 3 frames before and after it, along the motion of each block of 16 x 16 pixels.
 * The filtered frame n is ready when the frame n + 3 has been pushed, and the last 3 frames come out of [finish].
 *
 * The calls must be one after the other, because the frames have an order: a call during another call throws
 * `IllegalStateException`. A frame at 8K needs approximately 0.3 s on 12 cores.
 *
 * The denoiser holds native memory: close it after use (`use {}` in Kotlin, try-with-resources in Java). If you do not
 * close it, a `Cleaner` frees the memory after the garbage collector finds the denoiser.
 *
 * This class is for the JVM and Android. Kotlin: use the `suspend` forms. Java: each operation has a blocking form
 * (`push`) and a `CompletableFuture` form (`pushAsync`).
 *
 * @param width the width of a frame in pixels (even, at least 64).
 * @param height the height of a frame in pixels (even, at least 64).
 * @param strength multiplies the knee and the limit of the filter; 1 is the setting of the library.
 * @throws IllegalArgumentException if the size or the strength is wrong.
 */
class Denoiser @JvmOverloads constructor(
    val width: Int,
    val height: Int,
    strength: Float = 1.0f,
) : AutoCloseable {
    private val handle = NativeHandle(Native.denoiserNew(width, height, strength), "denoiser", Native::denoiserFree)
    private val cleanable = cleaner.register(this, handle)
    private val busy = AtomicBoolean(false)

    /** The number of bytes of a frame: `width * height * 3 / 2`. */
    val frameBytes: Int = Math.toIntExact(width.toLong() * height * 3 / 2)

    /** Adds the next frame. Gives the filtered frame when one is ready, else null. */
    @JvmSynthetic
    suspend fun push(frame: ByteArray): ByteArray? = withContext(Dispatchers.IO) { pushBlocking(frame) }

    /** See the `suspend` form. */
    @JvmName("push")
    fun pushBlocking(frame: ByteArray): ByteArray? = step { Native.denoiserPush(it, frame) }

    @JvmOverloads
    fun pushAsync(frame: ByteArray, executor: Executor = defaultExecutor): CompletableFuture<ByteArray?> =
        CompletableFuture.supplyAsync({ pushBlocking(frame) }, executor)

    /** Gives the next filtered frame after the last frame has been pushed, or null when there is none. Call it until it gives null. */
    @JvmSynthetic
    suspend fun finish(): ByteArray? = withContext(Dispatchers.IO) { finishBlocking() }

    /** See the `suspend` form. */
    @JvmName("finish")
    fun finishBlocking(): ByteArray? = step { Native.denoiserFinish(it) }

    @JvmOverloads
    fun finishAsync(executor: Executor = defaultExecutor): CompletableFuture<ByteArray?> =
        CompletableFuture.supplyAsync({ finishBlocking() }, executor)

    private fun step(block: (Long) -> ByteArray?): ByteArray? {
        check(busy.compareAndSet(false, true)) {
            "the previous call of the denoiser has not finished; await each call"
        }
        try {
            return handle.use(block)
        } finally {
            busy.set(false)
        }
    }

    /** Frees the native memory. It waits for the call that runs. A second `close()` does nothing. */
    override fun close() = cleanable.clean()
}
