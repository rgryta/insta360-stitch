package eu.gryta.insta360stitch

@JvmRecord
actual data class Size actual constructor(actual val width: Int, actual val height: Int)

@JvmRecord
actual data class PreviewImage actual constructor(actual val width: Int, actual val height: Int, actual val rgb: ByteArray)

@JvmRecord
actual data class VideoMaps actual constructor(
    actual val width: Int,
    actual val height: Int,
    actual val x0: ByteArray,
    actual val y0: ByteArray,
    actual val x1: ByteArray,
    actual val y1: ByteArray,
    actual val mask: ByteArray,
)
