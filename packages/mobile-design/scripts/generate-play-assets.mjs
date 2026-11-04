#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { inflateSync } from "node:zlib";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const outputDir = resolve(root, "apps/android/fastlane/metadata/android/en-US/images");
const logoPath = resolve(root, "apps/desktop/public/peppy-logo.svg");
const tokensPath = resolve(root, "packages/desktop-ui/src/peppy-tokens.css");
const assets = [
  { filename: "icon.png", width: 512, height: 512, logoWidth: 288 },
  { filename: "featureGraphic.png", width: 1024, height: 500 },
];

function token(css, name) {
  const value = css.match(new RegExp(`--peppy-${name}:\\s*(#[0-9a-fA-F]{6});`))?.[1];
  if (!value) throw new Error(`Missing --peppy-${name} color token`);
  return value;
}

function logoGeometry(source) {
  const viewBox = source.match(/viewBox="([^"]+)"/)?.[1];
  const path = source.match(/<path[^>]*\bd="([^"]+)"/)?.[1];
  const fill = source.match(/<path[^>]*\bfill="(#[0-9a-fA-F]{6})"/)?.[1];
  if (!viewBox || !path || !fill) throw new Error("Canonical logo is missing a view box, path, or fill");
  const [, , logoViewWidth, logoViewHeight] = viewBox.split(/\s+/).map(Number);
  if (!(logoViewWidth > 0 && logoViewHeight > 0)) throw new Error("Canonical logo has invalid dimensions");
  return { fill, logoViewHeight, logoViewWidth, path, viewBox };
}

function logoMarkup(geometry, canvasWidth, canvasHeight, logoWidth, background) {
  const { fill, logoViewHeight, logoViewWidth, path, viewBox } = geometry;
  const logoHeight = logoWidth * logoViewHeight / logoViewWidth;
  const x = (canvasWidth - logoWidth) / 2;
  const y = (canvasHeight - logoHeight) / 2;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${canvasWidth}" height="${canvasHeight}" viewBox="0 0 ${canvasWidth} ${canvasHeight}">
  <rect width="${canvasWidth}" height="${canvasHeight}" fill="${background}"/>
  <svg x="${x}" y="${y}" width="${logoWidth}" height="${logoHeight}" viewBox="${viewBox}">
    <path fill="${fill}" d="${path}"/>
  </svg>
</svg>
`;
}

function featureGraphicMarkup(geometry, background) {
  const { fill, logoViewHeight, logoViewWidth, path, viewBox } = geometry;
  const logoWidth = 250;
  const logoHeight = logoWidth * logoViewHeight / logoViewWidth;
  const logoX = 88;
  const logoY = (500 - logoHeight) / 2;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="500" viewBox="0 0 1024 500">
  <rect width="1024" height="500" fill="${background}"/>
  <svg x="${logoX}" y="${logoY}" width="${logoWidth}" height="${logoHeight}" viewBox="${viewBox}">
    <path fill="${fill}" d="${path}"/>
  </svg>
  <g fill="none" stroke="#FF8A73" stroke-linecap="round" stroke-linejoin="round" stroke-width="6">
    <rect x="500" y="120" width="104" height="210" rx="18"/>
    <rect x="748" y="148" width="184" height="128" rx="12"/>
    <path d="M544 360h132m-66-30v30m-154-135h260" stroke-dasharray="16 12"/>
    <path d="m716 195 22 0-11-11m11 11-11 11"/>
  </g>
  <g fill="#FFFFFF" font-family="Droid Sans, Arial, sans-serif" font-size="30" font-weight="700">
    <text x="500" y="402">Your phone's texts,</text>
    <text x="500" y="438">on your computer.</text>
  </g>
</svg>
`;
}

function pngChunks(png) {
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  if (!png.subarray(0, 8).equals(signature)) throw new Error("Expected a PNG");
  const chunks = [];
  for (let offset = 8; offset < png.length;) {
    if (offset + 12 > png.length) throw new Error("Truncated PNG chunk");
    const length = png.readUInt32BE(offset);
    const type = png.subarray(offset + 4, offset + 8).toString("ascii");
    const end = offset + 12 + length;
    if (end > png.length) throw new Error("Truncated PNG data");
    chunks.push({ type, data: png.subarray(offset + 8, offset + 8 + length) });
    offset = end;
  }
  return chunks;
}

function assertPng(png, expectedWidth, expectedHeight, requiredColorType) {
  const chunks = pngChunks(png);
  const header = chunks.find(({ type }) => type === "IHDR")?.data;
  if (!header || header.length !== 13) throw new Error("PNG is missing a valid header");
  const width = header.readUInt32BE(0);
  const height = header.readUInt32BE(4);
  const bitDepth = header[8];
  const colorType = header[9];
  if (width !== expectedWidth || height !== expectedHeight) throw new Error(`Expected ${expectedWidth}x${expectedHeight}, received ${width}x${height}`);
  if (bitDepth !== 8 || ![2, 6].includes(colorType) || header[12] !== 0) throw new Error("Expected a non-interlaced 8-bit RGB or RGBA PNG");
  if (requiredColorType !== undefined && colorType !== requiredColorType) throw new Error("Expected a 24-bit RGB PNG without an alpha channel");
  if (colorType === 2) return;
  const bytesPerPixel = 4;
  const rowLength = width * bytesPerPixel;
  const pixels = inflateSync(Buffer.concat(chunks.filter(({ type }) => type === "IDAT").map(({ data }) => data)));
  let previous = Buffer.alloc(rowLength);
  let offset = 0;
  for (let row = 0; row < height; row += 1) {
    const filter = pixels[offset++];
    const current = Buffer.from(pixels.subarray(offset, offset + rowLength));
    offset += rowLength;
    for (let column = 0; column < rowLength; column += 1) {
      const left = column >= bytesPerPixel ? current[column - bytesPerPixel] : 0;
      const up = previous[column];
      const upLeft = column >= bytesPerPixel ? previous[column - bytesPerPixel] : 0;
      if (filter === 1) current[column] = (current[column] + left) & 0xff;
      else if (filter === 2) current[column] = (current[column] + up) & 0xff;
      else if (filter === 3) current[column] = (current[column] + Math.floor((left + up) / 2)) & 0xff;
      else if (filter === 4) {
        const prediction = left + up - upLeft;
        const distances = [Math.abs(prediction - left), Math.abs(prediction - up), Math.abs(prediction - upLeft)];
        current[column] = (current[column] + [left, up, upLeft][distances.indexOf(Math.min(...distances))]) & 0xff;
      } else if (filter !== 0) throw new Error(`Unsupported PNG filter ${filter}`);
    }
    for (let column = 3; column < rowLength; column += bytesPerPixel) if (current[column] !== 255) throw new Error("Play icon must be fully opaque");
    previous = current;
  }
}

async function renderAssets(destination) {
  const [logo, tokens] = await Promise.all([readFile(logoPath, "utf8"), readFile(tokensPath, "utf8")]);
  const geometry = logoGeometry(logo);
  const temporary = await mkdtemp(join(tmpdir(), "peppy-play-assets-"));
  try {
    for (const asset of assets) {
      const svg = join(temporary, `${asset.filename}.svg`);
      const png = join(destination, asset.filename);
      const markup = asset.filename === "featureGraphic.png"
        ? featureGraphicMarkup(geometry, token(tokens, "surface-chrome"))
        : logoMarkup(geometry, asset.width, asset.height, asset.logoWidth, token(tokens, "surface-desk"));
      await writeFile(svg, markup);
      if (asset.filename === "featureGraphic.png") {
        const jpeg = join(temporary, "featureGraphic.jpg");
        execFileSync("sips", ["-s", "format", "jpeg", "-s", "formatOptions", "100", svg, "--out", jpeg], { stdio: "inherit" });
        execFileSync("sips", ["-s", "format", "png", jpeg, "--out", png], { stdio: "inherit" });
      } else execFileSync("sips", ["-z", String(asset.height), String(asset.width), "-s", "format", "png", svg, "--out", png], { stdio: "inherit" });
      assertPng(await readFile(png), asset.width, asset.height, asset.filename === "featureGraphic.png" ? 2 : undefined);
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function generate() {
  await mkdir(outputDir, { recursive: true });
  await renderAssets(outputDir);
}

async function check() {
  const temporary = await mkdtemp(join(tmpdir(), "peppy-play-assets-check-"));
  try {
    await renderAssets(temporary);
    for (const asset of assets) {
      const output = await readFile(join(outputDir, asset.filename));
      assertPng(output, asset.width, asset.height, asset.filename === "featureGraphic.png" ? 2 : undefined);
      if (!output.equals(await readFile(join(temporary, asset.filename)))) throw new Error(`Generated Play asset is stale: ${asset.filename}`);
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

if (process.argv.includes("--check")) await check();
else await generate();
