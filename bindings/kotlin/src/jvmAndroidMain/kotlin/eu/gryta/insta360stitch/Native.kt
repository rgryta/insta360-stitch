package eu.gryta.insta360stitch

/** Loads the JNI library: from the jar on the JVM, from the APK on Android. */
internal expect fun loadNativeLibrary()

/** The JNI functions of the Rust bridge (`bindings/kotlin/native/src/jni.rs`). A handle is a pointer to the open file. */
internal object Native {
    init {
        loadNativeLibrary()
    }

    @JvmStatic external fun open(path: String): Long

    @JvmStatic external fun free(handle: Long)

    @JvmStatic external fun unsupportedReason(handle: Long): String?

    @JvmStatic external fun localTime(handle: Long): String?

    @JvmStatic external fun offsetState(handle: Long): Int

    /** `[width, height]`, or null. */
    @JvmStatic external fun previewSize(handle: Long): IntArray?

    /** Writes `[width, height]` into [size] and gives the RGB data. */
    @JvmStatic external fun preview(handle: Long, size: IntArray): ByteArray

    /** Stitches the rows [firstRow] to `firstRow + rows - 1`. */
    @JvmStatic external fun stitch(
        handle: Long,
        raw: ByteArray,
        width: Int,
        height: Int,
        channels: Int,
        firstRow: Int,
        rows: Int,
    ): ByteArray

    /** Writes `[width, height]` into [size] and gives x0, y0, x1, y1 and the mask. [width] -1 is the default. */
    @JvmStatic external fun videoMaps(handle: Long, lensWidth: Int, lensHeight: Int, width: Int, size: IntArray): Array<ByteArray>

    @JvmStatic external fun addSphericalMetadata(input: String, output: String)

    /** `Int.MIN_VALUE` for null. */
    @JvmStatic external fun cameraUtcOffsetMinutes(localTime: String, utcUnixSeconds: Long): Int

    @JvmStatic external fun ffmpegFilter(): String
}
