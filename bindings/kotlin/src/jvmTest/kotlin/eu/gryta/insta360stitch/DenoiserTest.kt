package eu.gryta.insta360stitch

import kotlinx.coroutines.test.runTest
import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class DenoiserTest {
    private val w = 64
    private val h = 64

    /** A frame of 64 x 64 pixels: Y at [level] plus [noise], then flat U and V. */
    private fun frame(level: Int, noise: (Int) -> Double = { 0.0 }): ByteArray {
        val bytes = ByteArray(w * h * 3 / 2) { 128.toByte() }
        for (i in 0 until w * h) bytes[i] = (level + noise(i)).toInt().coerceIn(0, 255).toByte()
        return bytes
    }

    @Test
    fun everyFrameComesOutOnceInOrder() = runTest {
        Denoiser(w, h).use { denoiser ->
            assertEquals(w * h * 3 / 2, denoiser.frameBytes)
            val out = mutableListOf<ByteArray>()
            for (n in 0 until 6) {
                val filtered = denoiser.push(frame(100 + n))
                // The frame n - 3 is ready when the frame n has been pushed.
                assertEquals(n < 3, filtered == null, "frame $n")
                filtered?.let(out::add)
            }
            while (true) out.add(denoiser.finish() ?: break)
            assertEquals(6, out.size)
            assertNull(denoiser.finish())
            val levels = out.map { it[0].toInt() }
            assertEquals(levels.sorted(), levels)
            assertTrue(out.all { it.size == denoiser.frameBytes })
        }
    }

    @Test
    fun theNoiseOfAStaticSceneGoesDown() = runTest {
        val random = Random(5)
        val inputs = List(9) { frame(120) { random.nextDouble(-6.0, 6.0) } }
        Denoiser(w, h).use { denoiser ->
            val out = mutableListOf<ByteArray>()
            for (input in inputs) denoiser.push(input)?.let(out::add)
            while (true) out.add(denoiser.finish() ?: break)
            fun rms(f: ByteArray): Double {
                var sum = 0.0
                for (i in 0 until w * h) {
                    val d = (f[i].toInt() and 255) - 120
                    sum += d * d
                }
                return Math.sqrt(sum / (w * h))
            }
            assertTrue(rms(out[4]) < 0.75 * rms(inputs[4]), "${rms(inputs[4])} -> ${rms(out[4])}")
        }
    }

    @Test
    fun theFuturesGiveTheSameFramesAsTheBlockingCalls() {
        val frames = List(5) { frame(100 + it) }
        fun run(push: (ByteArray) -> ByteArray?, finish: () -> ByteArray?): List<ByteArray> {
            val out = frames.mapNotNull(push).toMutableList()
            while (true) out.add(finish() ?: break)
            return out
        }
        val expected = Denoiser(w, h).use { run(it::pushBlocking, it::finishBlocking) }
        val actual = Denoiser(w, h).use { run({ f -> it.pushAsync(f).get() }, { it.finishAsync().get() }) }
        assertEquals(expected.size, actual.size)
        expected.zip(actual).forEach { (a, b) -> assertContentEquals(a, b) }
    }

    @Test
    fun wrongArgumentsAndCallsAfterCloseAreErrors() = runTest {
        assertFailsWith<IllegalArgumentException> { Denoiser(30, 96) }
        assertFailsWith<IllegalArgumentException> { Denoiser(128, 96, 0f) }
        val denoiser = Denoiser(w, h, 2f)
        assertFailsWith<IllegalArgumentException> { denoiser.push(ByteArray(10)) }
        // The call that failed does not block the next call.
        assertNull(denoiser.push(frame(100)))
        denoiser.close()
        assertFailsWith<IllegalStateException> { denoiser.push(frame(101)) }
        denoiser.close()
    }

    @Test
    fun aCallDuringAnotherCallIsRefusedAndTheDenoiserStaysUsable() {
        // A frame of 2048 x 1024 takes long enough for the calls of two threads to overlap.
        val big = ByteArray(2048 * 1024 * 3 / 2) { (it % 251).toByte() }
        Denoiser(2048, 1024).use { denoiser ->
            val refused = java.util.concurrent.atomic.AtomicInteger()
            val failures = java.util.concurrent.ConcurrentLinkedQueue<Throwable>()
            val threads = List(2) {
                Thread {
                    repeat(8) {
                        try {
                            denoiser.pushBlocking(big)
                        } catch (e: IllegalStateException) {
                            if (e.message!!.contains("previous call")) refused.incrementAndGet() else failures.add(e)
                        } catch (e: Throwable) {
                            failures.add(e)
                        }
                    }
                }.also { it.start() }
            }
            threads.forEach { it.join() }
            assertTrue(failures.isEmpty(), "unexpected: ${failures.firstOrNull()}")
            assertTrue(refused.get() > 0, "no call was refused while another call ran")
            // Frames that were pushed come out; the denoiser still works after the refused calls.
            var rest = 0
            while (denoiser.finishBlocking() != null) rest++
            assertTrue(rest > 0)
        }
    }
}
