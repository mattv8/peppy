import { createRequire } from 'node:module';
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const SCRIPT_DIRECTORY = path.dirname(fileURLToPath(import.meta.url));
const DESKTOP_DIRECTORY = path.resolve(SCRIPT_DIRECTORY, '..');
const REPOSITORY_DIRECTORY = path.resolve(DESKTOP_DIRECTORY, '..', '..');
const DEFAULT_SOURCE = path.join(DESKTOP_DIRECTORY, 'public', 'peppy-logo.svg');
const DEFAULT_OUTPUT_DIRECTORY = path.join(DESKTOP_DIRECTORY, 'src-tauri', 'icons', 'tray');
const TAURI_REQUIRE = createRequire(path.join(DESKTOP_DIRECTORY, 'package.json'));
const TAURI_CLI = TAURI_REQUIRE.resolve('@tauri-apps/cli/tauri.js');
const SOURCE_FILL = '#e5533d';
const TRAY_VARIANTS = [
  { name: 'macos-template.png', size: 36, padding: 2, fill: '#000000' },
  { name: 'windows-color.png', size: 32, padding: 2, fill: SOURCE_FILL },
];

function attributeValue(attributes, name) {
  return new RegExp(`(?:^|\\s)${name}\\s*=\\s*["']([^"']+)["']`, 'i').exec(attributes)?.[1];
}

function parseViewBox(viewBox) {
  const values = viewBox.trim().split(/\s+/).map(Number);
  if (values.length !== 4 || values.some((value) => !Number.isFinite(value)) || values[2] <= 0 || values[3] <= 0) {
    throw new Error('Canonical SVG has an invalid viewBox.');
  }
  return values;
}

function formatNumber(value) {
  return Number(value.toFixed(6)).toString();
}

export function parseCanonicalIcon(svg) {
  const svgAttributes = /<svg\b([^>]*)>/i.exec(svg)?.[1];
  const paths = [...svg.matchAll(/<path\b([^>]*)\/?\s*>/gi)];

  if (!svgAttributes || paths.length !== 1 || /\btransform\s*=/i.test(svg)) {
    throw new Error('Canonical SVG must contain exactly one path.');
  }

  const viewBox = attributeValue(svgAttributes, 'viewBox');
  const pathData = attributeValue(paths[0][1], 'd');
  const fill = attributeValue(paths[0][1], 'fill')?.toLowerCase();

  if (!viewBox || !pathData || fill !== SOURCE_FILL) {
    throw new Error('Canonical SVG must have a viewBox, path data, and the Peppy coral fill.');
  }

  const [, , viewBoxWidth, viewBoxHeight] = parseViewBox(viewBox);
  return { viewBox, viewBoxWidth, viewBoxHeight, pathData, fill };
}

export function composePaddedSvg(icon, variant) {
  const drawableSize = variant.size - variant.padding * 2;
  const scale = Math.min(drawableSize / icon.viewBoxWidth, drawableSize / icon.viewBoxHeight);
  const width = icon.viewBoxWidth * scale;
  const height = icon.viewBoxHeight * scale;
  const x = (variant.size - width) / 2;
  const y = (variant.size - height) / 2;

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${variant.size}" height="${variant.size}" viewBox="0 0 ${variant.size} ${variant.size}"><svg x="${formatNumber(x)}" y="${formatNumber(y)}" width="${formatNumber(width)}" height="${formatNumber(height)}" viewBox="${icon.viewBox}"><path d="${icon.pathData}" fill="${variant.fill}"/></svg></svg>`;
}

function runTauriIcon(input, output, size) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [TAURI_CLI, 'icon', input, '--output', output, '--png', String(size)], {
      cwd: REPOSITORY_DIRECTORY,
      stdio: 'inherit',
    });
    child.once('error', reject);
    child.once('exit', (code) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`Tauri icon generation failed for ${size}px output (exit ${code}).`));
      }
    });
  });
}

async function generateVariants(icon, temporaryDirectory) {
  const generated = [];
  for (const variant of TRAY_VARIANTS) {
    const svgPath = path.join(temporaryDirectory, `${variant.size}.svg`);
    const iconDirectory = path.join(temporaryDirectory, `${variant.size}-output`);
    await writeFile(svgPath, composePaddedSvg(icon, variant));
    await runTauriIcon(svgPath, iconDirectory, variant.size);
    generated.push({ ...variant, path: path.join(iconDirectory, `${variant.size}x${variant.size}.png`) });
  }
  return generated;
}

async function assertGeneratedFilesMatch(generated, outputDirectory) {
  const mismatches = [];
  for (const generatedFile of generated) {
    const outputPath = path.join(outputDirectory, generatedFile.name);
    try {
      const [expected, actual] = await Promise.all([readFile(generatedFile.path), readFile(outputPath)]);
      if (!expected.equals(actual)) {
        mismatches.push(`stale ${generatedFile.name}`);
      }
    } catch {
      mismatches.push(`missing ${generatedFile.name}`);
    }
  }
  if (mismatches.length > 0) {
    throw new Error(`Tray icon outputs are ${mismatches.join(', ')}.`);
  }
}

export async function runGenerator({ check = false, source = DEFAULT_SOURCE, outputDirectory = DEFAULT_OUTPUT_DIRECTORY } = {}) {
  const temporaryDirectory = await mkdtemp(path.join(os.tmpdir(), 'peppy-tray-icons-'));
  try {
    const icon = parseCanonicalIcon(await readFile(source, 'utf8'));
    const generated = await generateVariants(icon, temporaryDirectory);
    if (check) {
      await assertGeneratedFilesMatch(generated, outputDirectory);
      return;
    }

    await mkdir(outputDirectory, { recursive: true });
    await Promise.all(generated.map((file) => copyFile(file.path, path.join(outputDirectory, file.name))));
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
}

if (import.meta.main) {
  const argumentsAfterCommand = process.argv.slice(2);
  if (argumentsAfterCommand.some((argument) => argument !== '--check')) {
    throw new Error('Usage: generate-tray-icons.mjs [--check]');
  }
  await runGenerator({ check: argumentsAfterCommand.includes('--check') });
}
