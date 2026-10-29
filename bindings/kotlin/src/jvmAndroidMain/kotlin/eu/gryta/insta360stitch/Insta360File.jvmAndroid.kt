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
