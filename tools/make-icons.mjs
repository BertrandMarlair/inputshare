// Generates the app icons from code, so they are reproducible rather than
// binary blobs nobody can regenerate.
//
//   node tools/make-icons.mjs
//
// Writes PNGs, a PNG-compressed .ico and a PNG-based .icns into
// crates/is-ui/icons/.

import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", "crates", "is-ui", "icons");

const ACCENT = [0x3f, 0x6c, 0xd4, 0xff];
const LIGHT = [0xff, 0xff, 0xff, 0xff];

// ------------------------------------------------------------------ drawing

function canvas(size) {
  return { size, px: new Uint8Array(size * size * 4) };
}

function blend(image, x, y, [r, g, b, a], coverage) {
  if (x < 0 || y < 0 || x >= image.size || y >= image.size) return;
  const alpha = (a / 255) * coverage;
  if (alpha <= 0) return;
  const i = (y * image.size + x) * 4;
  const existing = image.px[i + 3] / 255;
  const out = alpha + existing * (1 - alpha);
  for (let c = 0; c < 3; c++) {
    const src = [r, g, b][c];
    const dst = image.px[i + c];
    image.px[i + c] = Math.round((src * alpha + dst * existing * (1 - alpha)) / (out || 1));
  }
  image.px[i + 3] = Math.round(out * 255);
}

/** Signed distance to a rounded rectangle, for cheap analytic antialiasing. */
function roundRectDistance(x, y, x0, y0, x1, y1, radius) {
  const cx = (x0 + x1) / 2;
  const cy = (y0 + y1) / 2;
  const halfW = (x1 - x0) / 2 - radius;
  const halfH = (y1 - y0) / 2 - radius;
  const dx = Math.max(Math.abs(x - cx) - halfW, 0);
  const dy = Math.max(Math.abs(y - cy) - halfH, 0);
  return Math.hypot(dx, dy) - radius;
}

function roundRect(image, x0, y0, x1, y1, radius, color) {
  const from = Math.max(0, Math.floor(Math.min(y0, y1)) - 2);
  const to = Math.min(image.size, Math.ceil(Math.max(y0, y1)) + 2);
  for (let y = from; y < to; y++) {
    for (let x = 0; x < image.size; x++) {
      const distance = roundRectDistance(x + 0.5, y + 0.5, x0, y0, x1, y1, radius);
      const coverage = Math.min(Math.max(0.5 - distance, 0), 1);
      if (coverage > 0) blend(image, x, y, color, coverage);
    }
  }
}

/** The pointer that just crossed onto the second screen. */
function cursor(image, tipX, tipY, scale, color) {
  const outline = [
    [0, 0],
    [0, 3.1],
    [0.78, 2.35],
    [1.35, 3.6],
    [1.95, 3.32],
    [1.4, 2.08],
    [2.45, 2.0],
  ].map(([x, y]) => [tipX + x * scale, tipY + y * scale]);

  const xs = outline.map(([x]) => x);
  const ys = outline.map(([, y]) => y);
  const inside = (px, py) => {
    let hit = false;
    for (let i = 0, j = outline.length - 1; i < outline.length; j = i++) {
      const [xi, yi] = outline[i];
      const [xj, yj] = outline[j];
      if (yi > py !== yj > py && px < ((xj - xi) * (py - yi)) / (yj - yi) + xi) hit = !hit;
    }
    return hit;
  };

  const step = 0.34;
  for (let y = Math.floor(Math.min(...ys)); y <= Math.ceil(Math.max(...ys)); y++) {
    for (let x = Math.floor(Math.min(...xs)); x <= Math.ceil(Math.max(...xs)); x++) {
      let hits = 0;
      let samples = 0;
      for (let sy = step / 2; sy < 1; sy += step) {
        for (let sx = step / 2; sx < 1; sx += step) {
          samples++;
          if (inside(x + sx, y + sy)) hits++;
        }
      }
      if (hits) blend(image, x, y, color, hits / samples);
    }
  }
}

function draw(size) {
  const image = canvas(size);
  const u = size / 100;

  // Rounded app tile.
  roundRect(image, 4 * u, 4 * u, 96 * u, 96 * u, 22 * u, ACCENT);

  // Two screens, the left one handing over to the right one.
  roundRect(image, 16 * u, 30 * u, 50 * u, 56 * u, 3.5 * u, LIGHT);
  roundRect(image, 54 * u, 38 * u, 84 * u, 62 * u, 3.5 * u, LIGHT);

  // Stands, so they read as displays rather than windows.
  roundRect(image, 29 * u, 56 * u, 37 * u, 62 * u, 1.2 * u, LIGHT);
  roundRect(image, 24 * u, 62 * u, 42 * u, 65 * u, 1.5 * u, LIGHT);
  roundRect(image, 65 * u, 62 * u, 73 * u, 68 * u, 1.2 * u, LIGHT);
  roundRect(image, 60 * u, 68 * u, 78 * u, 71 * u, 1.5 * u, LIGHT);

  cursor(image, 62 * u, 42 * u, 4.2 * u, ACCENT);
  return image;
}

// --------------------------------------------------------------- png / ico

const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

function encodePng(image) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(image.size, 0);
  header.writeUInt32BE(image.size, 4);
  header[8] = 8; // bit depth
  header[9] = 6; // RGBA
  header[10] = 0;
  header[11] = 0;
  header[12] = 0;

  const stride = image.size * 4;
  const raw = Buffer.alloc((stride + 1) * image.size);
  for (let y = 0; y < image.size; y++) {
    raw[y * (stride + 1)] = 0; // filter: none
    Buffer.from(image.px.buffer, y * stride, stride).copy(raw, y * (stride + 1) + 1);
  }

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** A PNG-compressed ICO, which Windows has accepted since Vista. */
function encodeIco(entries) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(entries.length, 4);

  let offset = 6 + entries.length * 16;
  const directory = [];
  for (const { size, png } of entries) {
    const entry = Buffer.alloc(16);
    entry[0] = size >= 256 ? 0 : size;
    entry[1] = size >= 256 ? 0 : size;
    entry.writeUInt16LE(1, 4);
    entry.writeUInt16LE(32, 6);
    entry.writeUInt32LE(png.length, 8);
    entry.writeUInt32LE(offset, 12);
    directory.push(entry);
    offset += png.length;
  }

  return Buffer.concat([header, ...directory, ...entries.map((e) => e.png)]);
}

/** A PNG-based ICNS, which macOS has accepted since 10.7. */
function encodeIcns(entries) {
  const chunks = [];
  for (const { type, png } of entries) {
    const header = Buffer.alloc(8);
    header.write(type, 0, "ascii");
    // The length covers the header itself, which is the part everyone gets
    // wrong and which makes the file unreadable rather than merely ugly.
    header.writeUInt32BE(png.length + 8, 4);
    chunks.push(header, png);
  }
  const body = Buffer.concat(chunks);
  const file = Buffer.alloc(8);
  file.write("icns", 0, "ascii");
  file.writeUInt32BE(body.length + 8, 4);
  return Buffer.concat([file, body]);
}

// ------------------------------------------------------------------- output

mkdirSync(OUT, { recursive: true });

const pngSizes = [
  [32, "32x32.png"],
  [128, "128x128.png"],
  [256, "128x128@2x.png"],
  [512, "icon.png"],
];

for (const [size, name] of pngSizes) {
  writeFileSync(join(OUT, name), encodePng(draw(size)));
  console.log(`wrote ${name} (${size}x${size})`);
}

const icoSizes = [16, 32, 48, 64, 128, 256];
writeFileSync(
  join(OUT, "icon.ico"),
  encodeIco(icoSizes.map((size) => ({ size, png: encodePng(draw(size)) }))),
);
console.log(`wrote icon.ico (${icoSizes.join(", ")})`);

// The four-letter types are the sizes macOS asks for: a Dock icon, a Finder
// icon and their Retina doubles. An .icns missing the size the system wants
// falls back to a blurry scale of another one.
const icnsEntries = [
  ["ic11", 32], // 16pt @2x
  ["ic12", 64], // 32pt @2x
  ["ic07", 128],
  ["ic13", 256], // 128pt @2x
  ["ic08", 256],
  ["ic14", 512], // 256pt @2x
  ["ic09", 512],
  ["ic10", 1024], // 512pt @2x
];
writeFileSync(
  join(OUT, "icon.icns"),
  encodeIcns(icnsEntries.map(([type, size]) => ({ type, png: encodePng(draw(size)) }))),
);
console.log(`wrote icon.icns (${icnsEntries.map(([t, s]) => `${t}:${s}`).join(", ")})`);
