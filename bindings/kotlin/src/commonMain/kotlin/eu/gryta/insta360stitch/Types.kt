package eu.gryta.insta360stitch

/** A width and a height in pixels. */
data class Size(val width: Int, val height: Int)

/** The preview that the camera stitched. [rgb] is packed RGB. */
class PreviewImage(val width: Int, val height: Int, val rgb: ByteArray)

/**
 * The remap planes for ffmpeg. The output is [width] x [height].
 * [x0], [y0], [x1] and [y1] are little-endian `u16` planes (`gray16le`); the value 65535 is outside that lens.
 * [mask] is the blend weight of lens 0 (`gray`).
 */
class VideoMaps(
    val width: Int,
    val height: Int,
    val x0: ByteArray,
    val y0: ByteArray,
    val x1: ByteArray,
    val y1: ByteArray,
    val mask: ByteArray,
)
