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

    @JvmStatic external fun isPureVideo(handle: Long): Boolean

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

    /** A pointer to a new denoiser. [strength] multiplies the knee and the limit of the filter. */
    @JvmStatic external fun denoiserNew(width: Int, height: Int, strength: Float): Long

    @JvmStatic external fun denoiserFree(handle: Long)

    /** Pushes a frame. Gives the filtered frame, or null when none is ready. */
    @JvmStatic external fun denoiserPush(handle: Long, frame: ByteArray): ByteArray?

    /** Gives the next filtered frame after the last frame, or null when there is none. */
    @JvmStatic external fun denoiserFinish(handle: Long): ByteArray?

    @JvmStatic external fun addSphericalMetadata(input: String, output: String)

    /** `Int.MIN_VALUE` for null. */
    @JvmStatic external fun cameraUtcOffsetMinutes(localTime: String, utcUnixSeconds: Long): Int

    @JvmStatic external fun ffmpegFilter(): String

    /** Android only: reads the trailer from a file descriptor. The bridge keeps its own duplicate. */
    @JvmStatic external fun openFd(fd: Int): Long

    /** Android only: stitches [source] into [output] (both `ARGB_8888`, the same size), without a copy. */
    @JvmStatic external fun stitchBitmap(handle: Long, source: Any, output: Any)
}
