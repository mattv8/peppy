import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";

const root = resolve(import.meta.dirname, "../../..");

async function copyFileToFixture(fixture, file) {
  const destination = resolve(fixture, file);
  await mkdir(dirname(destination), { recursive: true });
  await cp(resolve(root, file), destination);
}

async function createIconFixture() {
  const fixture = await mkdtemp(join(tmpdir(), "peppy-icon-icns-test-"));
  await Promise.all([
    cp(resolve(root, "packages/mobile-design/assets"), resolve(fixture, "packages/mobile-design/assets"), { recursive: true }),
    cp(resolve(root, "packages/mobile-design/scripts"), resolve(fixture, "packages/mobile-design/scripts"), { recursive: true }),
    cp(resolve(root, "packages/mobile-design/src"), resolve(fixture, "packages/mobile-design/src"), { recursive: true }),
    copyFileToFixture(fixture, "packages/desktop-ui/src/peppy-tokens.css"),
    copyFileToFixture(fixture, "apps/desktop/public/peppy-logo.svg"),
    copyFileToFixture(fixture, "apps/desktop/package.json"),
  ]);
  const bin = resolve(fixture, "bin");
  await mkdir(bin);
  await writeFile(resolve(bin, "pnpm"), `#!/bin/sh
shift 4
exec ${JSON.stringify(resolve(root, "apps/desktop/node_modules/.bin/tauri"))} "$@"
`, { mode: 0o755 });
  await symlink(resolve(root, "apps/desktop/node_modules"), resolve(fixture, "apps/desktop/node_modules"));
  const icon = resolve(fixture, "apps/desktop/src-tauri/icons/icon.icns");
  await mkdir(dirname(icon), { recursive: true });
  await writeFile(icon, "stale icon");
  return { fixture, icon };
}

test("macOS icon generation replaces the desktop ICNS file", { skip: process.platform !== "darwin" }, async () => {
  const { fixture, icon } = await createIconFixture();
  try {
    execFileSync("node", ["packages/mobile-design/scripts/generate.mjs", "--icons"], { cwd: fixture, stdio: "inherit", env: { ...process.env, PATH: `${resolve(fixture, "bin")}:${process.env.PATH}` } });
    const generatedIcon = await readFile(icon);
    assert.equal(generatedIcon.subarray(0, 4).toString(), "icns");
    assert.ok(generatedIcon.length > 100, "generated ICNS must contain icon data");
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});
