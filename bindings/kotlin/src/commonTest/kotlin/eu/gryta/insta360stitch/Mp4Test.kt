package eu.gryta.insta360stitch

import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

/** A minimal MP4 file as ffmpeg writes it without faststart: ftyp, mdat, then moov with one video track. */
private fun minimalMp4(): ByteArray {
    fun u32(value: Int) = byteArrayOf((value ushr 24).toByte(), (value ushr 16).toByte(), (value ushr 8).toByte(), value.toByte())
    fun box(kind: String, vararg parts: ByteArray): ByteArray {
        val body = parts.fold(ByteArray(0)) { a, b -> a + b }
        return u32(8 + body.size) + kind.encodeToByteArray() + body
    }
    fun fullBox(kind: String, vararg parts: ByteArray) = box(kind, ByteArray(4), *parts)
    val ftyp = box("ftyp", "isom\u0000\u0000\u0002\u0000isomiso2".encodeToByteArray())
    val mdat = box("mdat", "VIDEO-ONE".encodeToByteArray())
    val entry = box("avc1", ByteArray(78), box("avcC", byteArrayOf(1, 2, 3)))
    val stbl = box("stbl", fullBox("stsd", u32(1), entry), fullBox("stco", u32(1) + u32(ftyp.size + 8)))
    val hdlr = fullBox("hdlr", ByteArray(4), "vide".encodeToByteArray(), ByteArray(13))
    val trak = box("trak", fullBox("tkhd", ByteArray(8)), box("mdia", hdlr, box("minf", stbl)))
    return ftyp + mdat + box("moov", fullBox("mvhd", ByteArray(8)), trak)
}

private fun ByteArray.kindAt(offset: Int) = decodeToString(offset + 4, offset + 8)

private fun ByteArray.u32At(offset: Int) =
    (this[offset].toInt() and 0xff shl 24) or (this[offset + 1].toInt() and 0xff shl 16) or
        (this[offset + 2].toInt() and 0xff shl 8) or (this[offset + 3].toInt() and 0xff)

class Mp4Test {
    @Test
    fun addsSphericalMetadata() = runTest {
        val input = tempFile("in.mp4", minimalMp4())
        val output = input.dropLast("in.mp4".length) + "out.mp4"
        addSphericalMetadata(input, output)
        val out = readFile(output)
        assertEquals("ftyp", out.kindAt(0))
        assertEquals("moov", out.kindAt(out.u32At(0)))
        val text = out.decodeToString()
        assertTrue("sv3d" in text && "st3d" in text && "GSpherical:Spherical>true" in text)
        assertFailsWith<IllegalArgumentException> { addSphericalMetadata(input, input) }
        // A bad input does not change the earlier output.
        writeFile(input, "not an mp4 file".encodeToByteArray())
        assertFailsWith<Insta360UnsupportedException> { addSphericalMetadata(input, output) }
        assertContentEquals(out, readFile(output))
    }
}
