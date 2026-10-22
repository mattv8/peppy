import { readFileSync } from "node:fs";
import { inflateSync } from "node:zlib";

const pngSignature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

function invalid(message) { throw new Error(`Invalid icon PNG: ${message}`); }

function unfilteredRgba(png) {
  if (!png.subarray(0, 8).equals(pngSignature)) invalid("missing PNG signature");
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  if (png[24] !== 8) invalid("expected 8-bit depth");
  if (png[25] !== 6) invalid("expected RGBA color type");
  if (png[28] !== 0) invalid("expected noninterlaced PNG");
  const compressed = [];
  for (let offset = 8; offset < png.length;) {
    const length = png.readUInt32BE(offset);
    const type = png.subarray(offset + 4, offset + 8).toString("ascii");
    if (type === "IDAT") compressed.push(png.subarray(offset + 8, offset + 8 + length));
    offset += length + 12;
  }
  const rows = inflateSync(Buffer.concat(compressed));
  const stride = width * 4;
  const pixels = Buffer.alloc(height * stride);
  for (let row = 0; row < height; row++) {
    const source = row * (stride + 1);
    const target = row * stride;
    for (let column = 0; column < stride; column++) {
      const left = column >= 4 ? pixels[target + column - 4] : 0;
      const above = row ? pixels[target - stride + column] : 0;
      const aboveLeft = row && column >= 4 ? pixels[target - stride + column - 4] : 0;
      const value = rows[source + column + 1];
      const filter = rows[source];
      if (filter === 0) pixels[target + column] = value;
      else if (filter === 1) pixels[target + column] = (value + left) & 255;
      else if (filter === 2) pixels[target + column] = (value + above) & 255;
      else if (filter === 3) pixels[target + column] = (value + Math.floor((left + above) / 2)) & 255;
      else if (filter === 4) {
        const p = left + above - aboveLeft;
        const pa = Math.abs(p - left), pb = Math.abs(p - above), pc = Math.abs(p - aboveLeft);
        pixels[target + column] = (value + (pa <= pb && pa <= pc ? left : pb <= pc ? above : aboveLeft)) & 255;
      } else invalid(`unsupported filter ${filter}`);
    }
  }
  return pixels;
}

export function icoFrameAlphas(file) {
  const ico = readFileSync(file);
  if (ico.readUInt16LE(2) !== 1) throw new Error("Invalid icon: expected ICO format");
  return Array.from({ length: ico.readUInt16LE(4) }, (_, index) => {
    const entry = 6 + index * 16;
    const length = ico.readUInt32LE(entry + 8);
    const offset = ico.readUInt32LE(entry + 12);
    return [...unfilteredRgba(ico.subarray(offset, offset + length)).filter((_, pixel) => pixel % 4 === 3)];
  });
}

export function verifyIcoTransparency(file) {
  const frames = icoFrameAlphas(file);
  if (!frames.length) throw new Error(`Invalid icon: no frames in ${file}`);
  for (const [index, alphas] of frames.entries()) {
    if (!alphas.includes(0) || !alphas.includes(255) || !alphas.some(alpha => alpha > 0 && alpha < 255)) {
      throw new Error(`Windows icon frame ${index + 1} must have transparent, opaque, and antialiased pixels`);
    }
  }
}
