package eu.gryta.insta360stitch

// The value types are Java records on the JVM and Android, thus they are expect classes here.

/** A width and a height in pixels. */
expect class Size(width: Int, height: Int) {
    val width: Int
    val height: Int
}

/** The preview that the camera stitched. [rgb] is packed RGB. `equals` compares the array by identity. */
expect class PreviewImage(width: Int, height: Int, rgb: ByteArray) {
    val width: Int
    val height: Int
    val rgb: ByteArray
}

/**
 * The remap planes for ffmpeg. The output is [width] x [height].
 * [x0], [y0], [x1] and [y1] are little-endian `u16` planes (`gray16le`); the value 65535 is outside that lens.
 * [mask] is the blend weight of lens 0 (`gray`). `equals` compares the arrays by identity.
 */
expect class VideoMaps(width: Int, height: Int, x0: ByteArray, y0: ByteArray, x1: ByteArray, y1: ByteArray, mask: ByteArray) {
    val width: Int
    val height: Int
    val x0: ByteArray
    val y0: ByteArray
    val x1: ByteArray
    val y1: ByteArray
    val mask: ByteArray
}
