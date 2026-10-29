package eu.gryta.insta360stitch

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executor

/** An Insta360 .insp or .insv file. See [Insta360FileBase] for the operations. */
actual class Insta360File private constructor(value: Long) : Insta360FileBase(value) {
    actual companion object {
        @JvmSynthetic
        actual suspend fun open(path: String): Insta360File = withContext(Dispatchers.IO) { openBlocking(path) }

        /** See the `suspend` form. */
        @JvmStatic
        @JvmName("open")
        @Throws(IOException::class)
        fun openBlocking(path: String): Insta360File = Insta360File(Native.open(path))

        @JvmStatic
        @JvmOverloads
        fun openAsync(path: String, executor: Executor = defaultExecutor): CompletableFuture<Insta360File> =
            CompletableFuture.supplyAsync({ openBlocking(path) }, executor)
    }
}
