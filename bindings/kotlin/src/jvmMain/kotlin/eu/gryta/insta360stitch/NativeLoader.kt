package eu.gryta.insta360stitch

import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

internal actual fun loadNativeLibrary() = NativeLoader.load()

/** Loads the JNI library on the JVM, from the jar resources. */
internal object NativeLoader {
    fun load() {
        // The tests give the library that cargo built for the host.
        System.getProperty("insta360stitch.library")?.let {
            System.load(it)
            return
        }
        val platform = platform()
        val name = System.mapLibraryName("insta360_stitch_jni")
        val bytes = NativeLoader::class.java.getResourceAsStream("/eu/gryta/insta360stitch/native/$platform/$name")
            ?.use { it.readBytes() }
            ?: throw UnsatisfiedLinkError("no native library for $platform in the insta360-stitch jar")
        // The name has the version and the content hash: two versions in one process do not collide, and a file
        // with this name always has this content.
        val hash = MessageDigest.getInstance("SHA-256").digest(bytes).take(8).joinToString("") { "%02x".format(it) }
        val target = File(System.getProperty("java.io.tmpdir"), "insta360_stitch_jni-$VERSION-$hash-$name")
        if (!target.isFile || target.length() != bytes.size.toLong()) {
            // Write a temporary file, then move it: another process can load the same file at the same time.
            val temporary = File.createTempFile("insta360_stitch_jni-", ".tmp", target.parentFile)
            temporary.writeBytes(bytes)
            try {
                Files.move(temporary.toPath(), target.toPath(), StandardCopyOption.ATOMIC_MOVE)
            } catch (_: java.nio.file.FileAlreadyExistsException) {
                // Another process was first. Its file has the same content.
                temporary.delete()
            }
        }
        System.load(target.absolutePath)
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
