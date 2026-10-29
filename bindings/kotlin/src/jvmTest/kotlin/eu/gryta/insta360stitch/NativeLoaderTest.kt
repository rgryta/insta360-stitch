package eu.gryta.insta360stitch

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals

class NativeLoaderTest {
    private val bytes = ByteArray(4096) { (it % 251).toByte() }

    @Test
    fun extractsOneTimeAndUsesTheFileAgain() {
        val folder = Files.createTempDirectory("loader-").toFile()
        val first = NativeLoader.extract(bytes, folder, "lib.so")
        assertContentEquals(bytes, first.readBytes())
        assertEquals(first, NativeLoader.extract(bytes, folder, "lib.so"))
        // Only the shared file stays: no temporary files are left.
        assertEquals(listOf(first.name), folder.list()!!.toList())
    }

    @Test
    fun doesNotLoadAFileWithOtherContent() {
        val folder = Files.createTempDirectory("loader-").toFile()
        val shared = NativeLoader.extract(bytes, folder, "lib.so")
        // A file with the same name and length, but other bytes (for example from another user in /tmp).
        shared.writeBytes(ByteArray(bytes.size))
        assertContentEquals(bytes, NativeLoader.extract(bytes, folder, "lib.so").readBytes())
    }
}
