package eu.gryta.insta360stitch

import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class StitchTest {
    private val pattern = ByteArray(64 * 32 * 3) { ((it * 7) % 251).toByte() }

    @Test
    fun stitchesInFullAndInBands() = runTest {
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            val whole = file.stitchPhoto(pattern, 64, 32, 3)
            assertEquals(pattern.size, whole.size)
            assertFailsWith<Insta360UnsupportedException> { file.stitchPhoto(ByteArray(27), 3, 3, 3) }
            // A band gives the same rows as the full stitch. A band after the last row is an error.
            assertContentEquals(whole.copyOfRange(10 * 64 * 3, 17 * 64 * 3), file.stitchPhotoBand(pattern, 64, 32, 3, 10, 7))
            assertFailsWith<IllegalArgumentException> { file.stitchPhotoBand(pattern, 64, 32, 3, 30, 7) }
            // A wrong buffer size is also a caller error, not an unsupported file.
            assertFailsWith<IllegalArgumentException> { file.stitchPhoto(ByteArray(10), 64, 32, 3) }
        }
    }

    @Test
    fun refusesNegativeAndHugeIntegers() = runTest {
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            // The core also refuses these values. The bridge check gives a clear message.
            val e = assertFailsWith<IllegalArgumentException> { file.stitchPhoto(pattern, -1, 32, 3) }
            assertEquals("invalid argument: width must not be negative, it is -1", e.message)
            assertFailsWith<IllegalArgumentException> { file.stitchPhoto(pattern, 64, 32, -3) }
            assertFailsWith<IllegalArgumentException> { file.stitchPhotoBand(pattern, 64, 32, 3, -1, 7) }
            assertFailsWith<IllegalArgumentException> { file.stitchPhotoBand(pattern, 64, 32, 3, 0, Int.MAX_VALUE) }
            assertFailsWith<IllegalArgumentException> { file.videoMaps(-64, 64) }
            assertFailsWith<IllegalArgumentException> { file.videoMaps(64, 64, -128) }
        }
    }

    @Test
    fun makesVideoMapsTwiceAsWideAsOneLens() = runTest {
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            val maps = file.videoMaps(64, 64)
            assertEquals(listOf(128, 64, 128 * 64, 128 * 64 * 2), listOf(maps.width, maps.height, maps.mask.size, maps.x0.size))
            val narrow = file.videoMaps(64, 64, 64)
            assertEquals(listOf(64, 32), listOf(narrow.width, narrow.height))
            for (width in listOf(126, 127)) {
                assertFailsWith<IllegalArgumentException> { file.videoMaps(64, 64, width) }
            }
        }
    }

    @Test
    fun exposesTheFilterAndTheCameraClock() {
        assertTrue("maskedmerge" in FFMPEG_FILTER)
        assertEquals(480, cameraUtcOffsetMinutes("20260821105307", 1_787_280_787))
        assertNull(cameraUtcOffsetMinutes("20260821050207", 1_787_280_787))
    }

    @Test
    fun readsThePreview() = runTest {
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            val preview = file.preview()
            assertEquals(listOf(2, 2, 2 * 2 * 3), listOf(preview.width, preview.height, preview.rgb.size))
        }
    }

    @Test
    fun readsThePreviewOfARealFile() = runTest {
        val fixtures = env("INSTA360_FIXTURES") ?: return@runTest
        Insta360File.open("$fixtures/IMG_20261023_053237_00_253.insp").use { file ->
            assertTrue(file.stitchable)
            assertEquals(0, file.offsetState)
            val preview = file.preview()
            assertEquals(preview.width * preview.height * 3, preview.rgb.size)
        }
    }
}
