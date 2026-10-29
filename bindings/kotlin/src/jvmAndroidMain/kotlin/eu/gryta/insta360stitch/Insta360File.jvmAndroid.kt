package eu.gryta.insta360stitch

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

actual class Insta360File private constructor(private var handle: Long) : AutoCloseable {
    private fun live(): Long = handle.also { check(it != 0L) { "the file is closed" } }

    actual val stitchable: Boolean get() = Native.unsupportedReason(live()) == null
    actual val unsupportedReason: String? get() = Native.unsupportedReason(live())
    actual val localTime: String? get() = Native.localTime(live())
    actual val offsetState: Int get() = Native.offsetState(live())
    actual val previewSize: Size? get() = Native.previewSize(live())?.let { Size(it[0], it[1]) }

    actual suspend fun preview(): PreviewImage = withContext(Dispatchers.IO) {
        val size = IntArray(2)
        val rgb = Native.preview(live(), size)
        PreviewImage(size[0], size[1], rgb)
    }

    actual suspend fun stitchPhoto(raw: ByteArray, width: Int, height: Int, channels: Int): ByteArray =
        withContext(Dispatchers.IO) { Native.stitch(live(), raw, width, height, channels, 0, height) }

    actual suspend fun stitchPhotoBand(
        raw: ByteArray,
        width: Int,
        height: Int,
        channels: Int,
        firstRow: Int,
        rows: Int,
    ): ByteArray = withContext(Dispatchers.IO) { Native.stitch(live(), raw, width, height, channels, firstRow, rows) }

    actual suspend fun videoMaps(lensWidth: Int, lensHeight: Int, width: Int?): VideoMaps = withContext(Dispatchers.IO) {
        // -1 is the default width in the bridge. A negative width from the caller is an error there.
        require(width != -1) { "invalid argument: width must not be negative, it is -1" }
        val size = IntArray(2)
        val (x0, y0, x1, y1, mask) = Native.videoMaps(live(), lensWidth, lensHeight, width ?: -1, size)
        VideoMaps(size[0], size[1], x0, y0, x1, y1, mask)
    }

    actual override fun close() {
        val value = handle
        handle = 0
        if (value != 0L) Native.free(value)
    }

    actual companion object {
        actual suspend fun open(path: String): Insta360File =
            withContext(Dispatchers.IO) { Insta360File(Native.open(path)) }
    }
}
