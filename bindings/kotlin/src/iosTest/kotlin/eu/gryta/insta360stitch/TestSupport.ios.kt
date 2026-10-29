package eu.gryta.insta360stitch

import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.convert
import kotlinx.cinterop.cstr
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.toKString
import kotlinx.cinterop.usePinned
import platform.Foundation.NSTemporaryDirectory
import platform.posix.SEEK_END
import platform.posix.fclose
import platform.posix.fopen
import platform.posix.fread
import platform.posix.fseek
import platform.posix.ftell
import platform.posix.fwrite
import platform.posix.getenv
import platform.posix.mkdtemp
import platform.posix.rewind

@OptIn(ExperimentalForeignApi::class)
actual fun tempFile(name: String, bytes: ByteArray): String {
    val folder = memScoped { mkdtemp("${NSTemporaryDirectory()}insta360-XXXXXX".cstr.getPointer(this))!!.toKString() }
    return "$folder/$name".also { writeFile(it, bytes) }
}

@OptIn(ExperimentalForeignApi::class)
actual fun env(name: String): String? = getenv(name)?.toKString()

@OptIn(ExperimentalForeignApi::class)
actual fun readFile(path: String): ByteArray {
    val file = fopen(path, "rb") ?: error("cannot open $path")
    try {
        fseek(file, 0, SEEK_END)
        val bytes = ByteArray(ftell(file).toInt())
        rewind(file)
        if (bytes.isNotEmpty()) bytes.usePinned { fread(it.addressOf(0), 1u, bytes.size.convert(), file) }
        return bytes
    } finally {
        fclose(file)
    }
}

@OptIn(ExperimentalForeignApi::class)
actual fun writeFile(path: String, bytes: ByteArray) {
    val file = fopen(path, "wb") ?: error("cannot open $path")
    try {
        if (bytes.isNotEmpty()) bytes.usePinned { fwrite(it.addressOf(0), 1u, bytes.size.convert(), file) }
    } finally {
        fclose(file)
    }
}
