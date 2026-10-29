package eu.gryta.insta360stitch

import android.graphics.Bitmap
import android.graphics.ColorSpace
import android.os.ParcelFileDescriptor
import android.system.Os
import android.system.OsConstants
import kotlinx.coroutines.test.runTest
import java.io.File
import java.nio.ByteBuffer
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class AndroidTest {
    // Alpha 255 in each pixel: an opaque source, as BitmapFactory gives for a JPEG. Premultiplication then changes nothing.
    private val rgba = ByteArray(64 * 32 * 4) { if (it % 4 == 3) -1 else ((it * 7) % 251).toByte() }

    private fun bitmap(config: Bitmap.Config = Bitmap.Config.ARGB_8888): Bitmap =
        Bitmap.createBitmap(64, 32, Bitmap.Config.ARGB_8888).apply { copyPixelsFromBuffer(ByteBuffer.wrap(rgba)) }
            .let { if (config == Bitmap.Config.ARGB_8888) it else it.copy(config, false) }

    @Test
    fun bitmapGivesTheSamePixelsAsTheByteArrayForm() = runTest {
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            val out = file.stitchPhoto(bitmap())
            assertEquals(listOf(64, 32, Bitmap.Config.ARGB_8888), listOf(out.width, out.height, out.config))
            val pixels = ByteBuffer.allocate(64 * 32 * 4).also { out.copyPixelsToBuffer(it) }.array()
            assertContentEquals(file.stitchPhoto(rgba, 64, 32, 4), pixels)
        }
    }

    @Test
    fun refusesAWrongBitmap() = runTest {
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            assertFailsWith<IllegalArgumentException> { file.stitchPhoto(bitmap(Bitmap.Config.RGB_565)) }
            // 4 bytes for each pixel, as ARGB_8888, but a different layout: only the format check refuses it.
            assertFailsWith<IllegalArgumentException> { file.stitchPhoto(bitmap(Bitmap.Config.RGBA_1010102)) }
            assertFailsWith<IllegalArgumentException> { file.stitchPhoto(bitmap(Bitmap.Config.HARDWARE)) }
            val recycled = bitmap().apply { recycle() }
            assertFailsWith<IllegalArgumentException> { file.stitchPhoto(recycled) }
            // The file still works after the errors.
            assertEquals(64, file.stitchPhoto(bitmap()).width)
        }
    }

    @Test
    fun outputKeepsTheColourSpaceOfTheSource() = runTest {
        val p3 = ColorSpace.get(ColorSpace.Named.DISPLAY_P3)
        val source = Bitmap.createBitmap(64, 32, Bitmap.Config.ARGB_8888, true, p3)
            .apply { copyPixelsFromBuffer(ByteBuffer.wrap(rgba)) }
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            assertEquals(p3, file.stitchPhoto(source).colorSpace)
        }
    }

    @Test
    fun openAndPreviewDoNotMoveTheFilePositionOfTheApp() = runTest {
        ParcelFileDescriptor.open(File(tempFile("IMG_1.insp", SYNTHETIC_X5)), ParcelFileDescriptor.MODE_READ_ONLY).use { pfd ->
            // The app reads from position 100 (for example BitmapFactory.decodeFileDescriptor). The library must not move it.
            Os.lseek(pfd.fileDescriptor, 100, OsConstants.SEEK_SET)
            val position = { Os.lseek(pfd.fileDescriptor, 0, OsConstants.SEEK_CUR) }
            Insta360File.open(pfd).use { file ->
                assertEquals(100L, position())
                assertEquals(2, file.preview().width)
                assertEquals(100L, position())
            }
        }
    }

    @Test
    fun opensAParcelFileDescriptorThatTheAppClosesLater() = runTest {
        val pfd = ParcelFileDescriptor.open(File(tempFile("IMG_1.insp", SYNTHETIC_X5)), ParcelFileDescriptor.MODE_READ_ONLY)
        val file = Insta360File.open(pfd)
        pfd.close()
        file.use {
            assertTrue(it.stitchable)
            // The preview reads the file again, from the duplicated descriptor.
            assertEquals(2, it.preview().width)
            assertEquals(2, it.preview().width)
        }
    }

    @Test
    fun javaFormsOfTheAndroidApi() {
        val pfd = ParcelFileDescriptor.open(File(tempFile("IMG_1.insp", SYNTHETIC_X5)), ParcelFileDescriptor.MODE_READ_ONLY)
        Insta360File.openAsync(pfd).get().use { file ->
            pfd.close()
            assertEquals(64, file.stitchPhotoBlocking(bitmap()).width)
            assertEquals(64, file.stitchPhotoAsync(bitmap()).get().width)
        }
    }
}
