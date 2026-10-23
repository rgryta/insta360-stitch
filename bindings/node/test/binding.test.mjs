import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
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

test('stitches a real photo off the event loop', { skip: !fixtures && 'set INSTA360_FIXTURES' }, async () => {
  const file = Insta360File.open(`${fixtures}/IMG_20261023_053237_00_253.insp`);
  assert.equal(file.stitchable, true);
  assert.equal(file.offsetState, 0);
  const { width, height, rgb } = file.preview();
  assert.equal(rgb.length, width * height * 3);
  // A synthetic 2:1 source is sufficient to test the async path and the size checks.
  const raw = Buffer.alloc(64 * 32 * 3, 128);
  const stitched = await file.stitchPhoto(raw, 64, 32, 3);
  assert.equal(stitched.length, raw.length);
  await assert.rejects(file.stitchPhoto(Buffer.alloc(27), 3, 3, 3), /unsupported/);
  // A band gives the same rows as the full stitch. A band after the last row is an error.
  const pattern = Buffer.from(Array.from({ length: 64 * 32 * 3 }, (_, i) => (i * 7) % 251));
  const whole = await file.stitchPhoto(pattern, 64, 32, 3);
  const band = await file.stitchPhotoBand(pattern, 64, 32, 3, 10, 7);
  assert.deepEqual(band, whole.subarray(10 * 64 * 3, 17 * 64 * 3));
  await assert.rejects(file.stitchPhotoBand(pattern, 64, 32, 3, 30, 7), /unsupported/);
});
