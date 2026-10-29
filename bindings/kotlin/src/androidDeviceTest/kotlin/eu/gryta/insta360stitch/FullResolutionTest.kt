package eu.gryta.insta360stitch

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Debug
import android.util.Log
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.test.runTest
import java.io.File
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.time.Duration.Companion.minutes
import kotlin.time.TimeSource

/**
 * Stitches a full-resolution X5 photo on the device, as an app does: BitmapFactory, stitchPhoto(Bitmap),
 * Bitmap.compress. It runs only with the instrumentation argument `fullResolution=<file name>`. Put the file into the
 * external files folder of the test app first (adb push). The test writes `<name>-stitched.jpg` next to it.
 */
class FullResolutionTest {
    @Test
    fun stitchesAFullResolutionPhoto() = runTest(timeout = 10.minutes) {
        val name = InstrumentationRegistry.getArguments().getString("fullResolution") ?: return@runTest
        val folder = InstrumentationRegistry.getInstrumentation().targetContext.getExternalFilesDir(null)!!
        val input = File(folder, name)
        // Sample the native heap during the work: the peak is the number that matters on a phone.
        var peak = 0L
        var sampling = true
        val sampler = thread {
            while (sampling) {
                peak = maxOf(peak, Debug.getNativeHeapAllocatedSize())
                Thread.sleep(20)
            }
        }
        val start = TimeSource.Monotonic.markNow()
        val source = BitmapFactory.decodeFile(input.path, BitmapFactory.Options().apply {
            inPreferredConfig = Bitmap.Config.ARGB_8888
        })
        val decoded = start.elapsedNow()
        val output = Insta360File.open(input.path).use { it.stitchPhoto(source) }
        val stitched = start.elapsedNow() - decoded
        source.recycle()
        val jpeg = File(folder, input.nameWithoutExtension + "-stitched.jpg")
        jpeg.outputStream().use { output.compress(Bitmap.CompressFormat.JPEG, 95, it) }
        val total = start.elapsedNow()
        sampling = false
        sampler.join()
        Log.i(
            "insta360-stitch",
            "full resolution ${output.width}x${output.height}: decode $decoded, stitch $stitched, total $total, " +
                "peak native heap ${peak / (1 shl 20)} MB",
        )
        assertEquals(2 * output.height, output.width)
    }
}
