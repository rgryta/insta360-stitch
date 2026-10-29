package eu.gryta.insta360stitch

/**
 * Copies the MP4 file at [input] to [output] and adds the 360 video metadata (Spherical Video V1 and V2).
 * It also moves the `moov` box to the start, the same as ffmpeg `-movflags +faststart`.
 * Give ffmpeg an output without `+faststart`, then use this function. [output] must be a different file.
 * The function writes a temporary file next to [output] and renames it. Thus, after an error, [output] did not change.
 * It refuses a fragmented MP4 file.
 */
expect suspend fun addSphericalMetadata(input: String, output: String)

/**
 * Gives the UTC offset of the camera in minutes, from the camera clock ([localTime], `YYYYMMDDhhmmss`) and the same
 * time in UTC. It gives null if the difference is not a real time zone. In that condition, the camera clock is wrong.
 */
expect fun cameraUtcOffsetMinutes(localTime: String, utcUnixSeconds: Long): Int?

/**
 * The ffmpeg `-filter_complex` for [Insta360File.videoMaps]. Input 0 is the .insv file.
 * Inputs 1 to 4 are x0, y0, x1 and y1 (`gray16le`). Input 5 is the mask (`gray`).
 */
expect val FFMPEG_FILTER: String
