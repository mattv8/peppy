import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  composePaddedSvg,
  parseCanonicalIcon,
  runGenerator,
} from './generate-tray-icons.mjs';

const CANONICAL_SVG = `<?xml version="1.0"?><svg width="80" height="40" viewBox="0 0 40 80"><path d="M0 0H40V80H0Z" fill="#e5533d"/></svg>`;
const MATCHING_PRESENTATION_SVG = `<?xml version="1.0"?><svg width="40" height="80" viewBox="0 0 40 80"><path d="M0 0H40V80H0Z" fill="#e5533d"/></svg>`;
const GENERATOR_PATH = fileURLToPath(new URL('./generate-tray-icons.mjs', import.meta.url));

function runGeneratorCli(scriptPath) {
  return spawnSync(process.execPath, [scriptPath, '--invalid'], { encoding: 'utf8' });
}

test('derives the canonical Peppy path geometry and source colour', async () => {
  const source = new URL('../public/peppy-logo.svg', import.meta.url);
  const icon = parseCanonicalIcon(await readFile(source, 'utf8'));

  assert.equal(icon.viewBoxWidth, 395.436);
  assert.equal(icon.viewBoxHeight, 397.016);
  assert.equal(icon.fill, '#e5533d');
  assert.match(icon.pathData, /^M112\.9 0/);
});

test('composes a centered padded square without a background tile', () => {
  const icon = parseCanonicalIcon(MATCHING_PRESENTATION_SVG);
  const svg = composePaddedSvg(icon, { size: 36, padding: 2, fill: '#000000' });

  assert.match(svg, /width="36" height="36" viewBox="0 0 36 36"/);
  assert.match(svg, /x="10" y="2" width="16" height="32"/);
  assert.match(svg, /fill="#000000"/);
  assert.doesNotMatch(svg, /<(?:rect|background)\b/);
});

test('uses viewBox dimensions rather than presentation dimensions for composition', () => {
  const icon = parseCanonicalIcon(CANONICAL_SVG);
  const svg = composePaddedSvg(icon, { size: 36, padding: 2, fill: '#000000' });

  assert.match(svg, /x="10" y="2" width="16" height="32"/);
});

test('rejects malformed, incomplete, and multi-path source SVGs', () => {
  assert.throws(() => parseCanonicalIcon('<svg><path fill="#e5533d" /></svg>'));
  assert.throws(() => parseCanonicalIcon('<svg width="1" height="1"><path d="M0 0" fill="#e5533d"/><path d="M0 0" fill="#e5533d"/></svg>'));
  assert.throws(() => parseCanonicalIcon('<svg width="1" height="1" viewBox="0 0 1 1"><path d="M0 0" fill="#e5533d" transform="scale(2)"/></svg>'));
  assert.throws(() => parseCanonicalIcon('<svg viewBox="0 0 1 nope"><path d="M0 0" fill="#e5533d"/></svg>'));
  assert.throws(() => parseCanonicalIcon('<svg data-viewBox="0 0 1 1"><path d="M0 0" fill="#e5533d"/></svg>'));
  assert.throws(() => parseCanonicalIcon('<svg viewBox="0 0 1 1"><path data-d="M0 0" fill="#e5533d"/></svg>'));
  assert.throws(() => parseCanonicalIcon('<svg viewBox="0 0 1 1"><path d="M0 0" fill="#000000"/></svg>'));
});

test('rejects invalid CLI arguments when invoked directly', () => {
  const result = runGeneratorCli(GENERATOR_PATH);

  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Usage: generate-tray-icons\.mjs/);
});

test('rejects invalid CLI arguments when invoked through a symlink', async (t) => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peppy-tray-icons-test-'));
  const linkedGeneratorPath = path.join(directory, 'generate-tray-icons.mjs');

  try {
    try {
      await symlink(GENERATOR_PATH, linkedGeneratorPath, 'file');
    } catch (error) {
      if (process.platform === 'win32' && ['EACCES', 'EPERM'].includes(error.code)) {
        t.skip('host does not permit symlink creation');
        return;
      }
      throw error;
    }

    const result = runGeneratorCli(linkedGeneratorPath);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Usage: generate-tray-icons\.mjs/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('check reports missing outputs without writing them', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peppy-tray-icons-test-'));
  const source = path.join(directory, 'logo.svg');
  const outputDirectory = path.join(directory, 'output');
  await writeFile(source, CANONICAL_SVG);

  try {
    await assert.rejects(runGenerator({ check: true, source, outputDirectory }), /missing/);
    await assert.rejects(readFile(path.join(outputDirectory, 'macos-template.png')));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('generates deterministic PNGs and detects stale committed bytes', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peppy-tray-icons-test-'));
  const source = path.join(directory, 'logo.svg');
  const outputDirectory = path.join(directory, 'output');
  await writeFile(source, CANONICAL_SVG);

  try {
    await runGenerator({ source, outputDirectory });
    const firstMac = await readFile(path.join(outputDirectory, 'macos-template.png'));
    const firstWindows = await readFile(path.join(outputDirectory, 'windows-color.png'));

    await runGenerator({ source, outputDirectory });
    assert.deepEqual(await readFile(path.join(outputDirectory, 'macos-template.png')), firstMac);
    assert.deepEqual(await readFile(path.join(outputDirectory, 'windows-color.png')), firstWindows);

    await writeFile(path.join(outputDirectory, 'windows-color.png'), 'stale');
    await assert.rejects(runGenerator({ check: true, source, outputDirectory }), /stale/);
    assert.equal(await readFile(path.join(outputDirectory, 'windows-color.png'), 'utf8'), 'stale');
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
