package eu.gryta.insta360stitch

/** Writes `bytes` to the file `name` in a new temporary folder and gives the path. */
expect fun tempFile(name: String, bytes: ByteArray): String

/** Gives the value of the environment variable `name`, or null. */
expect fun env(name: String): String?
