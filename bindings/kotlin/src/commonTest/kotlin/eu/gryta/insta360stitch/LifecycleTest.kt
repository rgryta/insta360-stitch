package eu.gryta.insta360stitch

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.TimeSource

class LifecycleTest {
    // A stitch of this size takes long enough to call close() while it runs.
    private val big = ByteArray(4096 * 2048 * 3) { (it % 251).toByte() }

    @Test
    fun callAfterCloseThrows() = runTest {
        val file = Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5))
        file.close()
        assertFailsWith<IllegalStateException> { file.stitchable }
        assertFailsWith<IllegalStateException> { file.stitchPhoto(ByteArray(64 * 32 * 3), 64, 32, 3) }
        file.close()
    }

    @Test
    fun closeWaitsForARunningStitch() = runTest {
        val file = Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5))
        withContext(Dispatchers.Default) {
            val stitch = async { file.stitchPhoto(big, 4096, 2048, 3) }
            // Let the stitch start, then close the file under it.
            val start = TimeSource.Monotonic.markNow()
            Thread.sleep(50)
            assertFalse(stitch.isCompleted, "the stitch was complete before close(): use a larger photo")
            file.close()
            val closed = start.elapsedNow()
            assertEquals(big.size, stitch.await().size)
            // close() waited for the stitch: the stitch ends very soon after close() returns.
            val total = start.elapsedNow()
            assertTrue(total - closed < 100.milliseconds, "close() returned at $closed, the stitch ended at $total")
        }
        assertFailsWith<IllegalStateException> { file.stitchable }
    }

    @Test
    fun cancelledCallDoesNotBreakTheFile() = runTest {
        Insta360File.open(tempFile("IMG_1.insp", SYNTHETIC_X5)).use { file ->
            withContext(Dispatchers.Default) {
                assertNull(withTimeoutOrNull(1) { file.stitchPhoto(big, 4096, 2048, 3) })
            }
            assertEquals(64 * 32 * 3, file.stitchPhoto(ByteArray(64 * 32 * 3), 64, 32, 3).size)
        }
    }
}
