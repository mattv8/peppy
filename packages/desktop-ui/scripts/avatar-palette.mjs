#!/usr/bin/env node
// Resampling is opt-in because floating-point math is not pinned across V8 versions.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ENTRY_COUNT = 120;
const MIN_CONTRAST = 4.5;
const MIN_DELTA_E = 0.028;
const WHITE = [1, 1, 1];
const DARK_GLYPH = [0x1a / 255, 0x1a / 255, 0x1a / 255].map(toLinear);
const SCRIPT_DIRECTORY = dirname(fileURLToPath(import.meta.url));
const PACKAGE_DIRECTORY = dirname(SCRIPT_DIRECTORY);
const PALETTE_PATH = join(PACKAGE_DIRECTORY, "avatar-palette.json");
const CSS_PATH = join(PACKAGE_DIRECTORY, "src", "avatar-palette.generated.css");
const RUST_PATH = join(
  PACKAGE_DIRECTORY,
  "..",
  "..",
  "apps",
  "desktop",
  "src-tauri",
  "src",
  "head_palette.rs",
);

function toLinear(channel) {
  return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
}

function toGamma(channel) {
  return channel <= 0.0031308
    ? 12.92 * channel
    : 1.055 * channel ** (1 / 2.4) - 0.055;
}

function oklchToRgb(lightness, chroma, hue) {
  const a = chroma * Math.cos((hue * Math.PI) / 180);
  const b = chroma * Math.sin((hue * Math.PI) / 180);
  const lPrime = lightness + 0.3963377774 * a + 0.2158037573 * b;
  const mPrime = lightness - 0.1055613458 * a - 0.0638541728 * b;
  const sPrime = lightness - 0.0894841775 * a - 1.291485548 * b;
  const l = lPrime ** 3;
  const m = mPrime ** 3;
  const s = sPrime ** 3;

  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

function rgbToOklab(rgb) {
  const l = Math.cbrt(0.4122214708 * rgb[0] + 0.5363325363 * rgb[1] + 0.0514459929 * rgb[2]);
  const m = Math.cbrt(0.2119034982 * rgb[0] + 0.6806995451 * rgb[1] + 0.1073969566 * rgb[2]);
  const s = Math.cbrt(0.0883024619 * rgb[0] + 0.2817188376 * rgb[1] + 0.6299787005 * rgb[2]);

  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

function luminance(rgb) {
  return 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
}

function contrastRatio(first, second) {
  const lighter = Math.max(first, second);
  const darker = Math.min(first, second);
  return (lighter + 0.05) / (darker + 0.05);
}

function deltaE(first, second) {
  return Math.hypot(first[0] - second[0], first[1] - second[1], first[2] - second[2]);
}

function hexToLinearRgb(hex) {
  return [1, 3, 5].map((offset) => toLinear(Number.parseInt(hex.slice(offset, offset + 2), 16) / 255));
}

function rgbToHex(rgb) {
  return `#${rgb
    .map((channel) => Math.round(toGamma(Math.min(1, Math.max(0, channel))) * 255).toString(16).padStart(2, "0"))
    .join("")}`;
}

function isInGamut(rgb) {
  return rgb.every((channel) => channel >= -0.0001 && channel <= 1.0001);
}

function validCandidate(lightness, chroma, hue, glyphLuminance) {
  const rgb = oklchToRgb(lightness, chroma, hue);
  if (!isInGamut(rgb)) {
    return null;
  }

  const hex = rgbToHex(rgb);
  return contrastRatio(luminance(hexToLinearRgb(hex)), glyphLuminance) >= 4.55 ? hex : null;
}

function resamplePalette() {
  const candidates = [];
  for (let t = 0; t <= 1.0001; t += 0.05) {
    for (let hue = 0; hue < 360; hue += 3) {
      for (let chroma = 0.06; chroma <= 0.15 + 1e-9; chroma += 0.01) {
        const lightness = 0.42 + 0.18 * t;
        const light = validCandidate(lightness, chroma, hue, luminance(WHITE));
        const dark = validCandidate(0.74 + 0.14 * t, chroma * 0.9, hue, luminance(DARK_GLYPH));
        if (light && dark) {
          candidates.push({
            hex: [light, dark],
            lab: [rgbToOklab(hexToLinearRgb(light)), rgbToOklab(hexToLinearRgb(dark))],
            hue,
            lightness,
          });
        }
      }
    }
  }

  const seed = candidates.reduce((best, candidate) =>
    Math.abs(candidate.hue - 33) + Math.abs(candidate.lightness - 0.52) <
    Math.abs(best.hue - 33) + Math.abs(best.lightness - 0.52)
      ? candidate
      : best,
  );
  const selected = [seed];
  const distances = candidates.map((candidate) => candidateDistance(candidate, seed));

  for (let count = 1; count < ENTRY_COUNT; count += 1) {
    let bestIndex = 0;
    for (let index = 1; index < candidates.length; index += 1) {
      if (distances[index] > distances[bestIndex]) {
        bestIndex = index;
      }
    }
    const next = candidates[bestIndex];
    selected.push(next);
    for (let index = 0; index < candidates.length; index += 1) {
      distances[index] = Math.min(distances[index], candidateDistance(candidates[index], next));
    }
  }

  return selected.map((candidate) => candidate.hex);
}

function candidateDistance(first, second) {
  return Math.min(deltaE(first.lab[0], second.lab[0]), deltaE(first.lab[1], second.lab[1]));
}

function readPalette() {
  return JSON.parse(readFileSync(PALETTE_PATH, "utf8"));
}

function validatePalette(palette) {
  const errors = [];
  if (!Array.isArray(palette) || palette.length !== ENTRY_COUNT) {
    return [`expected exactly ${ENTRY_COUNT} palette entries`];
  }

  const pairs = new Set();
  for (const [index, entry] of palette.entries()) {
    if (!Array.isArray(entry) || entry.length !== 2 || !entry.every((hex) => typeof hex === "string")) {
      errors.push(`entry ${index} must be a [lightHex, darkHex] pair`);
      continue;
    }
    if (!entry.every((hex) => /^#[0-9a-f]{6}$/.test(hex))) {
      errors.push(`entry ${index} must use lowercase #rrggbb hex`);
      continue;
    }
    const key = entry.join(",");
    if (pairs.has(key)) {
      errors.push(`entry ${index} duplicates a palette pair`);
    }
    pairs.add(key);
  }
  if (errors.length > 0) {
    return errors;
  }

  const themes = [
    { name: "light", colors: palette.map(([light]) => light), glyph: luminance(WHITE) },
    { name: "dark", colors: palette.map(([, dark]) => dark), glyph: luminance(DARK_GLYPH) },
  ];
  for (const theme of themes) {
    const labs = theme.colors.map((hex) => rgbToOklab(hexToLinearRgb(hex)));
    for (const [index, hex] of theme.colors.entries()) {
      const ratio = contrastRatio(luminance(hexToLinearRgb(hex)), theme.glyph);
      if (ratio < MIN_CONTRAST) {
        errors.push(`${theme.name} entry ${index} contrast ${ratio.toFixed(3)} is below ${MIN_CONTRAST}`);
      }
    }
    for (let first = 0; first < labs.length; first += 1) {
      for (let second = first + 1; second < labs.length; second += 1) {
        const distance = deltaE(labs[first], labs[second]);
        if (distance < MIN_DELTA_E) {
          errors.push(
            `${theme.name} entries ${first} and ${second} ΔE ${distance.toFixed(4)} is below ${MIN_DELTA_E}`,
          );
        }
      }
    }
  }
  return errors;
}

function renderCss(palette) {
  const darkValues = palette.map(([, dark], index) => `  --peppy-avatar-${index}: ${dark};`);
  const lightValues = palette.map(([light], index) => `  --peppy-avatar-${index}: ${light};`);
  return [
    "/* Generated by packages/desktop-ui/scripts/avatar-palette.mjs from avatar-palette.json; do not edit. */",
    "",
    ".theme-system,",
    ".theme-dark {",
    "  --peppy-avatar-glyph: #1a1a1a;",
    ...darkValues,
    "}",
    "",
    ".theme-light {",
    "  --peppy-avatar-glyph: #fff;",
    ...lightValues,
    "}",
    "",
    "@media (prefers-color-scheme: light) {",
    "  .theme-system {",
    "    --peppy-avatar-glyph: #fff;",
    ...lightValues.map((value) => `  ${value}`),
    "  }",
    "}",
    "",
  ].join("\n");
}

function renderRust(palette) {
  const colors = palette.map(([light]) => {
    const channels = [1, 3, 5].map((offset) => `0x${light.slice(offset, offset + 2)}`);
    return `    [${channels.join(", ")}],`;
  });
  return [
    "//! Generated by packages/desktop-ui/scripts/avatar-palette.mjs from avatar-palette.json; do not edit.",
    "",
    `pub const HEAD_COLORS: [[u8; 3]; ${ENTRY_COUNT}] = [`,
    ...colors,
    "];",
    "",
    "pub fn head_color(index: usize) -> [u8; 3] {",
    "    HEAD_COLORS[index % HEAD_COLORS.len()]",
    "}",
    "",
  ].join("\n");
}

function writeOutputs(palette) {
  writeFileSync(CSS_PATH, renderCss(palette));
  writeFileSync(RUST_PATH, renderRust(palette));
}

function checkOutput(path, expected, errors) {
  try {
    if (readFileSync(path, "utf8") !== expected) {
      errors.push(`${path} is out of date; run node packages/desktop-ui/scripts/avatar-palette.mjs`);
    }
  } catch {
    errors.push(`${path} is missing; run node packages/desktop-ui/scripts/avatar-palette.mjs`);
  }
}

function main() {
  const argument = process.argv[2];
  if (argument && argument !== "--check" && argument !== "--resample") {
    throw new Error("usage: avatar-palette.mjs [--check|--resample]");
  }

  const palette = argument === "--resample" ? resamplePalette() : readPalette();
  const validationErrors = validatePalette(palette);
  if (validationErrors.length > 0) {
    throw new Error(`avatar palette validation failed:\n${validationErrors.join("\n")}`);
  }

  if (argument === "--resample") {
    writeFileSync(PALETTE_PATH, `${JSON.stringify(palette, null, 2)}\n`);
  }
  if (argument === "--check") {
    const errors = [];
    checkOutput(CSS_PATH, renderCss(palette), errors);
    checkOutput(RUST_PATH, renderRust(palette), errors);
    if (errors.length > 0) {
      throw new Error(`avatar palette check failed:\n${errors.join("\n")}`);
    }
    console.log(`Avatar palette check passed (${ENTRY_COUNT} entries).`);
    return;
  }

  writeOutputs(palette);
  console.log(`Generated avatar palette (${ENTRY_COUNT} entries).`);
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
