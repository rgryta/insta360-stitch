package eu.gryta.insta360stitch

/** The AAR has the library for each ABI in `jni/<abi>/`. */
internal actual fun loadNativeLibrary() = System.loadLibrary("insta360_stitch_jni")
