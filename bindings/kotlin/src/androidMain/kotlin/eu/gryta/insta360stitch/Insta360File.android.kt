package eu.gryta.insta360stitch

import android.graphics.Bitmap
import android.graphics.ColorSpace
import android.os.ParcelFileDescriptor
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executor

/**
 * An Insta360 .insp or .insv file. See [Insta360FileBase] for the operations.
 *
 * Android also has:
 * - [open] with a `ParcelFileDescriptor`, for `content://` files.
 * - [stitchPhoto] with a `Bitmap`: the stitch reads and writes the bitmap pixels directly. A full-resolution X5 photo
 *   then needs no pixel buffer on the Java heap.
 */
actual class Insta360File private constructor(value: Long) : Insta360FileBase(value) {
    @JvmSynthetic
    suspend fun stitchPhoto(source: Bitmap): Bitmap = withContext(Dispatchers.IO) { stitchPhotoBlocking(source) }

    /**
     * Stitches a photo from a decoded bitmap, for example from `BitmapFactory.decodeFile`, into a new bitmap of the
     * same size. The source must be an opaque `ARGB_8888` software bitmap (a decoded .insp is opaque). The output is
     * opaque too.
     * @throws IllegalArgumentException for a different config, a `HARDWARE` bitmap or a recycled bitmap.
     */
    @JvmName("stitchPhoto")
    fun stitchPhotoBlocking(source: Bitmap): Bitmap {
        // The bridge checks the config, the hardware flag and the stride.
        // A recycled bitmap has no colour space to read.
        require(!source.isRecycled) { "invalid argument: the source bitmap is recycled" }
        // The output has the colour space of the source (for example Display P3), else sRGB.
        val colorSpace = source.colorSpace ?: ColorSpace.get(ColorSpace.Named.SRGB)
        val output = Bitmap.createBitmap(source.width, source.height, Bitmap.Config.ARGB_8888, true, colorSpace)
        handle.use { Native.stitchBitmap(it, source, output) }
        return output
    }

    @JvmOverloads
    fun stitchPhotoAsync(source: Bitmap, executor: Executor = defaultExecutor): CompletableFuture<Bitmap> =
        CompletableFuture.supplyAsync({ stitchPhotoBlocking(source) }, executor)

    // The path forms are the same as in jvmMain/Insta360File.jvm.kt: change both. Java needs them on this class.
    actual companion object {
        @JvmSynthetic
        actual suspend fun open(path: String): Insta360File = withContext(Dispatchers.IO) { openBlocking(path) }

        /** See the `suspend` form. */
        @JvmStatic
        @JvmName("open")
        @Throws(IOException::class)
        fun openBlocking(path: String): Insta360File = Insta360File(Native.open(path))

        @JvmStatic
        @JvmOverloads
        fun openAsync(path: String, executor: Executor = defaultExecutor): CompletableFuture<Insta360File> =
            CompletableFuture.supplyAsync({ openBlocking(path) }, executor)

        /**
         * Reads the trailer from an open file, for example from `contentResolver.openFileDescriptor(uri, "r")`.
         * The library keeps its own duplicate of the descriptor: you can close [file] after this call.
         */
        @JvmSynthetic
        suspend fun open(file: ParcelFileDescriptor): Insta360File = withContext(Dispatchers.IO) { openBlocking(file) }

        /** See the `suspend` form. */
        @JvmStatic
        @JvmName("open")
        @Throws(IOException::class)
        fun openBlocking(file: ParcelFileDescriptor): Insta360File = Insta360File(Native.openFd(file.fd))

        @JvmStatic
        @JvmOverloads
        fun openAsync(file: ParcelFileDescriptor, executor: Executor = defaultExecutor): CompletableFuture<Insta360File> =
            CompletableFuture.supplyAsync({ openBlocking(file) }, executor)
    }
}
