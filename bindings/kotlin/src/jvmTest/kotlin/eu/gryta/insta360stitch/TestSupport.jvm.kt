package eu.gryta.insta360stitch

import java.nio.file.Files

actual fun tempFile(name: String, bytes: ByteArray): String =
    Files.createTempDirectory("insta360-").resolve(name).also { Files.write(it, bytes) }.toString()

actual fun env(name: String): String? = System.getenv(name)

actual fun readFile(path: String): ByteArray = java.io.File(path).readBytes()

actual fun writeFile(path: String, bytes: ByteArray) = java.io.File(path).writeBytes(bytes)
