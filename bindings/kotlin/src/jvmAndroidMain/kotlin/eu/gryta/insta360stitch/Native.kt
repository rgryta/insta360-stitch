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
}
