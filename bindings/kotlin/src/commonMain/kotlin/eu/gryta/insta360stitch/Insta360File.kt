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

    /** Gives the small equirectangular preview that the camera stitched. */
    suspend fun preview(): PreviewImage

    /**
     * Stitches the decoded raw pixels of the photo into an equirectangular image.
     * The two lenses are side by side (`width = 2 * height`). The output has the same size and channels (1 to 4).
     * @throws IllegalArgumentException if the size of [raw] is not `width * height * channels`.
     */
    suspend fun stitchPhoto(raw: ByteArray, width: Int, height: Int, channels: Int): ByteArray

    /**
     * Stitches only the output rows [firstRow] to `firstRow + rows - 1` of the photo.
     * Use it to stitch a photo in parts, for example to show progress or to limit memory.
     * The other arguments are the same as for [stitchPhoto].
     */
    suspend fun stitchPhotoBand(raw: ByteArray, width: Int, height: Int, channels: Int, firstRow: Int, rows: Int): ByteArray

    /**
     * Makes the remap planes for a .insv file with lens streams of [lensWidth] x [lensHeight].
     * The output is `width` x `width / 2`. The default width is `2 * lensWidth`, which keeps the resolution of the lenses.
     * [width] must be a multiple of 4 and at most 16384. Give the planes to ffmpeg with [FFMPEG_FILTER].
     */
    suspend fun videoMaps(lensWidth: Int, lensHeight: Int, width: Int? = null): VideoMaps

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
