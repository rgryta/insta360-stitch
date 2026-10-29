package eu.gryta.insta360stitch

import java.io.File
import java.io.IOException
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
        System.load(extract(bytes, File(System.getProperty("java.io.tmpdir")), name).absolutePath)
    }

    /**
     * Gives a file in [folder] with [bytes]. A file of an earlier start is used again only if it has the same owner and
     * the same content: in a shared `/tmp`, another user can make a file with this name first.
     */
    fun extract(bytes: ByteArray, folder: File, name: String): File {
        // The name has the version and a part of the content hash: two versions in one process do not collide.
        val hash = sha256(bytes)
        val target = File(folder, "insta360_stitch_jni-$VERSION-${hash.take(16)}-$name")
        // A new private file (createTempFile gives mode 0600 on POSIX). Its owner is the user of this process.
        val fresh = Files.createTempFile(folder.toPath(), "insta360_stitch_jni-", "-$name")
        val owner = Files.getOwner(fresh)
        fun usable() = target.isFile && Files.getOwner(target.toPath()) == owner && sha256(target.readBytes()) == hash
        if (usable()) {
            Files.delete(fresh)
            return target
        }
        Files.write(fresh, bytes)
        try {
            Files.move(fresh, target.toPath(), StandardCopyOption.ATOMIC_MOVE)
            return target
        } catch (_: IOException) {
            // The move fails if the file of another user is there, or (Windows) if another process loaded it.
            if (usable()) {
                Files.delete(fresh)
                return target
            }
            return fresh.toFile().also { it.deleteOnExit() }
        }
    }

    private fun sha256(bytes: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

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
