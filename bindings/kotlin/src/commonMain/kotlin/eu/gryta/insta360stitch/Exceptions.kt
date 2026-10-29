package eu.gryta.insta360stitch

/**
 * The library cannot stitch the file, because of the camera, the lens accessory or the trailer layout.
 * A retry will not help. Show the preview from the camera instead.
 */
class Insta360UnsupportedException(
    /** Why the library cannot stitch the file. */
    val reason: String,
) : RuntimeException("unsupported: $reason")

/** A read operation failed, or it gave fewer bytes than necessary. A retry can help. On the JVM and Android, this is `java.io.IOException`. */
expect open class IOException(message: String?) : Exception
