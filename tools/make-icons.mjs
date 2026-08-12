#!/usr/bin/env node
/**
 * Draw the application icon.
 *
 * Run with `node tools/make-icons.mjs`. Nothing imports this; it writes the
 * PNGs that `tauri.conf.json` lists under `bundle.icon`.
 *
 * The icon is generated rather than checked in as a binary because it is a few
 * shapes and a palette, and a script is reviewable in a way a binary is not —
 * change the accent in one place here and every size follows.
 *
 * The mark is a glass on an amber ground: the accent colour of the interface,
 * and a shape that still reads at sixteen pixels when there is nothing else to
 * go on.
 */

import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const OUT_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "icons");

const AMBER = [0xe9, 0xa6, 0x3f];
const INK = [0x14, 0x16, 0x1d];

/** CRC-32, which PNG requires on every chunk. */
const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(data.length, 0);
  head.write(type, 4, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])), 0);
  return Buffer.concat([head, data, crc]);
}

function encodePng(size, pixels) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(size, 0);
  header.writeUInt32BE(size, 4);
  header[8] = 8; // bit depth
  header[9] = 6; // truecolour with alpha
  // 10..12 stay zero: deflate, adaptive filtering, no interlace.

  // One filter byte per scanline, then the row. Filter 0 (none) keeps this
  // readable; the images are tiny and compression is not the point.
  const stride = size * 4;
  const raw = Buffer.alloc((stride + 1) * size);
  for (let y = 0; y < size; y += 1) {
    pixels.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/**
 * The mark, drawn in a unit square so every size is the same picture.
 *
 * Sampled four times per axis and averaged, which is what stops the diagonals
 * of the glass looking like a staircase at 32 pixels.
 */
function shadeAt(u, v) {
  // Rounded square ground.
  const r = 0.22;
  const dx = Math.max(Math.abs(u - 0.5) - (0.5 - r), 0);
  const dy = Math.max(Math.abs(v - 0.5) - (0.5 - r), 0);
  if (Math.hypot(dx, dy) > r) return null;

  // A glass: a bowl tapering to a stem, then a foot.
  const bowlTop = 0.24;
  const bowlBottom = 0.56;
  const stemBottom = 0.72;
  const footTop = 0.72;
  const footBottom = 0.78;

  if (v >= bowlTop && v <= bowlBottom) {
    // Width shrinks linearly from the rim to the point where the stem starts.
    const t = (v - bowlTop) / (bowlBottom - bowlTop);
    const halfWidth = 0.24 * (1 - t) + 0.022;
    if (Math.abs(u - 0.5) <= halfWidth) return INK;
  }
  if (v > bowlBottom && v <= stemBottom && Math.abs(u - 0.5) <= 0.032) return INK;
  if (v >= footTop && v <= footBottom && Math.abs(u - 0.5) <= 0.17) return INK;

  return AMBER;
}

function render(size) {
  const pixels = Buffer.alloc(size * size * 4);
  const SAMPLES = 4;
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      let r = 0;
      let g = 0;
      let b = 0;
      let a = 0;
      for (let sy = 0; sy < SAMPLES; sy += 1) {
        for (let sx = 0; sx < SAMPLES; sx += 1) {
          const colour = shadeAt(
            (x + (sx + 0.5) / SAMPLES) / size,
            (y + (sy + 0.5) / SAMPLES) / size,
          );
          if (colour) {
            r += colour[0];
            g += colour[1];
            b += colour[2];
            a += 255;
          }
        }
      }
      const total = SAMPLES * SAMPLES;
      const offset = (y * size + x) * 4;
      // Averaging the colour across uncovered samples would darken the edge
      // pixels, so the colour is divided by the covered samples only and the
      // alpha by all of them.
      const covered = a / 255 || 1;
      pixels[offset] = Math.round(r / covered);
      pixels[offset + 1] = Math.round(g / covered);
      pixels[offset + 2] = Math.round(b / covered);
      pixels[offset + 3] = Math.round(a / total);
    }
  }
  return encodePng(size, pixels);
}

mkdirSync(OUT_DIR, { recursive: true });
for (const [name, size] of [
  ["32x32.png", 32],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["icon.png", 512],
]) {
  writeFileSync(join(OUT_DIR, name), render(size));
  console.log(`wrote icons/${name}`);
}
