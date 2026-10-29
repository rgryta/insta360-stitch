package eu.gryta.insta360stitch

import java.nio.file.Files

actual fun tempFile(name: String, bytes: ByteArray): String =
    Files.createTempDirectory("insta360-").resolve(name).also { Files.write(it, bytes) }.toString()

actual fun env(name: String): String? = System.getenv(name)
