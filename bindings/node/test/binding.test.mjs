import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

const { Insta360File, FFMPEG_FILTER, cameraUtcOffsetMinutes } = createRequire(import.meta.url)('../index.js');
const fixtures = process.env.INSTA360_FIXTURES;

test('exposes the ffmpeg filter and the camera clock helper', () => {
  assert.match(FFMPEG_FILTER, /maskedmerge/);
  const utc = Date.UTC(2026, 7, 21, 2, 53, 7) / 1000;
  assert.equal(cameraUtcOffsetMinutes('20260821105307', utc), 480);
  assert.equal(cameraUtcOffsetMinutes('20260821050207', utc), null);
});

test('tells an unreadable file from an unsupported one', () => {
  assert.throws(() => Insta360File.open('/nonexistent/IMG_1.insp'), /^Error: i\/o error/);
  assert.throws(() => Insta360File.open(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), /unsupported: no Insta360 trailer/);
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
  const file = Insta360File.open(syntheticX5());
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

test('reads the preview of a real file', { skip: !fixtures && 'set INSTA360_FIXTURES' }, () => {
  const file = Insta360File.open(`${fixtures}/IMG_20261023_053237_00_253.insp`);
  assert.equal(file.stitchable, true);
  assert.equal(file.offsetState, 0);
  const { width, height, rgb } = file.preview();
  assert.equal(rgb.length, width * height * 3);
});
