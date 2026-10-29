package eu.gryta.insta360stitch

/**
 * An Insta360 .insp or .insv file. The open operation reads its trailer and calculates the level rotation one time.
 * The file holds native memory: close it after use (`use {}` in Kotlin, try-with-resources in Java).
 */
expect class Insta360File : AutoCloseable {
    /** Is `true` if the library supports the calibration, the lens guard and the IMU data. */
    val stitchable: Boolean

    /** The reason why the library cannot stitch the file, or null. */
    val unsupportedReason: String?

    /** The time on the camera clock when the camera made the file, as `YYYYMMDDhhmmss`. */
    val localTime: String?

    /** The state of the lens guard or accessory: 0 = none, 5 = premium guard. */
    val offsetState: Int

    /** The width and height of the preview, or null if the trailer has no preview. */
    val previewSize: Size?

    /** Frees the native memory. A call after `close()` throws `IllegalStateException`. A second `close()` does nothing. */
    override fun close()

    companion object {
        /**
         * Reads the trailer. It reads only the records that the stitcher needs, and not the image or video data.
         * @throws IOException if the read fails.
         * @throws Insta360UnsupportedException if the file has no trailer that the library can read.
         */
        suspend fun open(path: String): Insta360File
    }
}
