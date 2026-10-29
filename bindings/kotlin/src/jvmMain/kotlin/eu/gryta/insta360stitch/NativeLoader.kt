package eu.gryta.insta360stitch

internal actual fun loadNativeLibrary() = NativeLoader.load()

/** Loads the JNI library on the JVM. */
internal object NativeLoader {
    fun load() {
        // The tests give the library that cargo built for the host.
        val explicit = System.getProperty("insta360stitch.library")
        if (explicit != null) {
            System.load(explicit)
            return
        }
        error("no native library for ${platform()}")
    }

    /** `<os>-<arch>`, for example `linux-x86_64`. */
    fun platform(): String {
        val os = System.getProperty("os.name").lowercase()
        val name = when {
            os.startsWith("windows") -> "windows"
            os.startsWith("mac") -> "macos"
            os.startsWith("linux") -> "linux"
            else -> os
        }
        val arch = when (val a = System.getProperty("os.arch").lowercase()) {
            "amd64", "x86_64" -> "x86_64"
            "aarch64", "arm64" -> "aarch64"
            else -> a
        }
        return "$name-$arch"
    }
}
