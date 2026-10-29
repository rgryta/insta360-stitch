package eu.gryta.insta360stitch

import java.lang.ref.Cleaner
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write

/** Frees the native files that the caller did not close. One daemon thread for the library. */
internal val cleaner: Cleaner = Cleaner.create()

/**
 * The pointer to an open native file. Each call holds the read lock. [run] frees the pointer with the write lock:
 * thus it waits for the calls that run, and a call after it throws `IllegalStateException`.
 * The Cleaner holds this object, not the `Insta360File`, thus the file can become unreachable.
 */
internal class NativeHandle(private var value: Long) : Runnable {
    private val lock = ReentrantReadWriteLock()

    fun <T> use(block: (Long) -> T): T = lock.read {
        check(value != 0L) { "the file is closed" }
        block(value)
    }

    override fun run() = lock.write {
        if (value != 0L) {
            Native.free(value)
            value = 0
        }
    }
}
