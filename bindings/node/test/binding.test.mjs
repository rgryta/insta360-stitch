import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

const { Insta360File, FFMPEG_FILTER, addSphericalMetadata, cameraUtcOffsetMinutes } = createRequire(import.meta.url)(
  // The release check sets this to an installed package. Then the tests check what a user gets from npm.
  process.env.INSTA360_STITCH_MODULE ?? '../index.js',
);
const fixtures = process.env.INSTA360_FIXTURES;

test('exposes the ffmpeg filter and the camera clock helper', () => {
  assert.match(FFMPEG_FILTER, /maskedmerge/);
  const utc = Date.UTC(2026, 7, 21, 2, 53, 7) / 1000;
  assert.equal(cameraUtcOffsetMinutes('20260821105307', utc), 480);
  assert.equal(cameraUtcOffsetMinutes('20260821050207', utc), null);
});

test('opens a file off the event loop, and tells an unreadable file from an unsupported one', async () => {
  const opening = Insta360File.open(syntheticX5());
  assert.ok(opening instanceof Promise);
  assert.ok((await opening).stitchable);
  await assert.rejects(Insta360File.open('/nonexistent/IMG_1.insp'), /^Error: i\/o error/);
  await assert.rejects(Insta360File.open(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), /unsupported: no Insta360 trailer/);
});

// Functions that make a synthetic X5 file: a stand-in image, then a trailer with calibration and IMU data.
const varint = (value) => {
  const out = [];
  let v = BigInt(value);
  do {
    const byte = Number(v % 128n);
    v /= 128n;
    out.push(v > 0n ? byte | 0x80 : byte);
  } while (v > 0n);
  return Buffer.from(out);
};
const field = (number, value) =>
  Buffer.isBuffer(value)
    ? Buffer.concat([varint(number * 8 + 2), varint(value.length), value])
    : Buffer.concat([varint(number * 8), varint(value)]);
const record = (id, payload) => {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(id, 0);
  header.writeUInt32LE(payload.length, 2);
  return Buffer.concat([payload, header]);
};
const OFFSET =
  '2_2645.947_2700.400_2678.170_0.717_0.092_89.898_2646.118_8071.690_2674.010_-1.108_0.204_90.231_10752_5376_1137';
const syntheticX5 = () => {
  const imu = Buffer.alloc(40);
  for (let i = 0; i < 2; i++) {
    imu.writeBigUInt64LE(BigInt(1_000_000 + i * 1000), i * 20);
    for (const [k, value] of [0, 0, -1024, 0, 0, 0].entries()) imu.writeUInt16LE(32_768 + value, i * 20 + 8 + k * 2);
  }
  const metadata = Buffer.concat([field(5, Buffer.from(OFFSET)), field(7, 20_260_821_105_307n)]);
  const trailer = Buffer.concat([record(0x300, imu), record(0x101, metadata), Buffer.alloc(72)]);
  trailer.writeUInt32LE(trailer.length, trailer.length - 78 + 38);
  trailer.write('8db42d694ccc418790edff439fe026bf', trailer.length - 32, 'latin1');
  const path = join(mkdtempSync(join(tmpdir(), 'insta360-')), 'IMG_1.insp');
  writeFileSync(path, Buffer.concat([Buffer.from('stand-in image'), trailer]));
  return path;
};

test('stitches off the event loop, in full and in bands', async () => {
  const file = await Insta360File.open(syntheticX5());
  assert.equal(file.stitchable, true);
  assert.equal(file.localTime, '20260821105307');
  const pattern = Buffer.from(Array.from({ length: 64 * 32 * 3 }, (_, i) => (i * 7) % 251));
  const whole = await file.stitchPhoto(pattern, 64, 32, 3);
  assert.equal(whole.length, pattern.length);
  await assert.rejects(file.stitchPhoto(Buffer.alloc(27), 3, 3, 3), /unsupported/);
  // A band gives the same rows as the full stitch. A band after the last row is an error, also a very large one.
  const band = await file.stitchPhotoBand(pattern, 64, 32, 3, 10, 7);
  assert.deepEqual(band, whole.subarray(10 * 64 * 3, 17 * 64 * 3));
  await assert.rejects(file.stitchPhotoBand(pattern, 64, 32, 3, 30, 7), /^Error: invalid argument/);
  await assert.rejects(file.stitchPhotoBand(pattern, 64, 32, 3, 0, 4_000_000_000), /^Error: invalid argument/);
  // A wrong buffer size is also a caller error, not an unsupported file.
  await assert.rejects(file.stitchPhoto(Buffer.alloc(10), 64, 32, 3), /^Error: invalid argument/);
});

// A minimal MP4 file as ffmpeg writes it without faststart: ftyp, mdat, then moov with one video track.
const box = (kind, ...parts) => {
  const body = Buffer.concat(parts);
  const header = Buffer.alloc(8);
  header.writeUInt32BE(8 + body.length, 0);
  header.write(kind, 4, 'latin1');
  return Buffer.concat([header, body]);
};
const fullBox = (kind, ...parts) => box(kind, Buffer.alloc(4), ...parts);
const minimalMp4 = () => {
  const ftyp = box('ftyp', Buffer.from('isom\0\0\x02\0isomiso2', 'latin1'));
  const mdat = box('mdat', Buffer.from('VIDEO-ONE'));
  const offsets = Buffer.alloc(8);
  offsets.writeUInt32BE(1, 0);
  offsets.writeUInt32BE(ftyp.length + 8, 4);
  const entry = box('avc1', Buffer.alloc(78), box('avcC', Buffer.from([1, 2, 3])));
  const count = Buffer.alloc(4);
  count.writeUInt32BE(1, 0);
  const stbl = box('stbl', fullBox('stsd', count, entry), fullBox('stco', offsets));
  const hdlr = fullBox('hdlr', Buffer.alloc(4), Buffer.from('vide'), Buffer.alloc(13));
  const trak = box('trak', fullBox('tkhd', Buffer.alloc(8)), box('mdia', hdlr, box('minf', stbl)));
  return Buffer.concat([ftyp, mdat, box('moov', fullBox('mvhd', Buffer.alloc(8)), trak)]);
};

test('makes video maps twice as wide as one lens by default', async () => {
  const file = await Insta360File.open(syntheticX5());
  const maps = await file.videoMaps(64, 64);
  assert.deepEqual([maps.width, maps.height, maps.mask.length, maps.x0.length], [128, 64, 128 * 64, 128 * 64 * 2]);
  const narrow = await file.videoMaps(64, 64, 64);
  assert.deepEqual([narrow.width, narrow.height], [64, 32]);
  for (const width of [126, 127]) {
    await assert.rejects(file.videoMaps(64, 64, width), /^Error: invalid argument/);
  }
});

test('adds 360 video metadata to an MP4 file', async () => {
  const folder = mkdtempSync(join(tmpdir(), 'insta360-'));
  const [input, output] = [join(folder, 'in.mp4'), join(folder, 'out.mp4')];
  writeFileSync(input, minimalMp4());
  await addSphericalMetadata(input, output);
  const out = readFileSync(output);
  assert.equal(out.toString('latin1', 4, 8), 'ftyp');
  assert.equal(out.toString('latin1', out.readUInt32BE(0) + 4, out.readUInt32BE(0) + 8), 'moov');
  assert.ok(out.includes('sv3d') && out.includes('st3d') && out.includes('GSpherical:Spherical>true'));
  await assert.rejects(addSphericalMetadata(input, input), /^Error: invalid argument/);
  // A bad input does not change the earlier output.
  writeFileSync(input, 'not an mp4 file');
  await assert.rejects(addSphericalMetadata(input, output), /^Error: unsupported/);
  assert.deepEqual(readFileSync(output), out);
});

test('reads the preview of a real file', { skip: !fixtures && 'set INSTA360_FIXTURES' }, async () => {
  const file = await Insta360File.open(`${fixtures}/IMG_20261023_053237_00_253.insp`);
  assert.equal(file.stitchable, true);
  assert.equal(file.offsetState, 0);
  const { width, height, rgb } = file.preview();
  assert.equal(rgb.length, width * height * 3);
});
