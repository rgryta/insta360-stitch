package eu.gryta.insta360stitch

import eu.gryta.insta360stitch.cinterop.I360_INVALID_ARGUMENT
import eu.gryta.insta360stitch.cinterop.I360_IO
import eu.gryta.insta360stitch.cinterop.I360_OK
import eu.gryta.insta360stitch.cinterop.I360_UNSUPPORTED
import eu.gryta.insta360stitch.cinterop.i360_string_free
import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.CPointerVar
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.MemScope
import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.toKString
import kotlinx.cinterop.value

actual open class IOException actual constructor(message: String?) : Exception(message)

actual data class Size actual constructor(actual val width: Int, actual val height: Int)

actual class PreviewImage actual constructor(actual val width: Int, actual val height: Int, actual val rgb: ByteArray)

actual class VideoMaps actual constructor(
    actual val width: Int,
    actual val height: Int,
    actual val x0: ByteArray,
    actual val y0: ByteArray,
    actual val x1: ByteArray,
    actual val y1: ByteArray,
    actual val mask: ByteArray,
)

/** Takes a string that the C API allocated, and frees it. */
@OptIn(ExperimentalForeignApi::class)
internal fun CPointer<ByteVar>?.takeString(): String? = this?.let {
    val text = it.toKString()
    i360_string_free(it)
    text
}

/**
 * Calls a C API function with an error output, and throws the exception for its status, the same as on the JVM:
 * unsupported, i/o, invalid argument, or an internal error.
 */
@OptIn(ExperimentalForeignApi::class)
internal inline fun <T> checked(block: MemScope.(error: CPointer<CPointerVar<ByteVar>>) -> Pair<Int, T>): T = memScoped {
    val error = alloc<CPointerVar<ByteVar>>()
    val (status, value) = block(error.ptr)
    val message = error.value.takeString() ?: ""
    when (status) {
        I360_OK -> value
        I360_UNSUPPORTED -> throw Insta360UnsupportedException(message.removePrefix("unsupported: "))
        I360_IO -> throw IOException(message)
        I360_INVALID_ARGUMENT -> throw IllegalArgumentException(message)
        else -> throw IllegalStateException(message)
    }
}
