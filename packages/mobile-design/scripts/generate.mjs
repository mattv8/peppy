#!/usr/bin/env node
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { catalog, nativeCatalog, desktopCatalog } from "../src/catalog.mjs";
import { kotlinNames, semanticTokens, swiftNames } from "../src/tokens.mjs";
import { verifyIcoTransparency } from "./icon-alpha.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const paths = {
  css: resolve(root, "packages/desktop-ui/src/peppy-tokens.css"),
  mark: resolve(root, "apps/desktop/public/peppy-logo.svg"),
  appIcon: resolve(root, "packages/mobile-design/assets/peppy-app-icon.svg"),
  windowsAppIcon: resolve(root, "packages/mobile-design/assets/peppy-windows-app-icon.svg"),
  darkAppIcon: resolve(root, "packages/mobile-design/assets/peppy-app-icon-dark.svg"),
  tintedAppIcon: resolve(root, "packages/mobile-design/assets/peppy-app-icon-tinted.svg"),
  androidTokens: resolve(root, "apps/android/app/src/main/java/dev/peppy/mobile/ui/theme/PeppyTokens.kt"),
  androidStrings: resolve(root, "apps/android/app/src/main/res/values/strings_peppy.xml"),
  androidLogo: resolve(root, "apps/android/app/src/main/res/drawable/peppy_logo.xml"),
  androidForeground: resolve(root, "apps/android/app/src/main/res/drawable/ic_launcher_foreground.xml"),
  androidBackground: resolve(root, "apps/android/app/src/main/res/drawable/ic_launcher_background.xml"),
  androidMonochrome: resolve(root, "apps/android/app/src/main/res/drawable/ic_launcher_monochrome.xml"),
  androidIcon: resolve(root, "apps/android/app/src/main/res/mipmap-anydpi-v26/ic_launcher.xml"),
  androidRoundIcon: resolve(root, "apps/android/app/src/main/res/mipmap-anydpi-v26/ic_launcher_round.xml"),
  iosRoot: resolve(root, "apps/ios/PeppyMobile/Assets.xcassets/Contents.json"),
  iosTokens: resolve(root, "apps/ios/PeppyMobile/Design/PeppyTokens.swift"),
  iosStrings: resolve(root, "apps/ios/PeppyMobile/Peppy.xcstrings"),
  desktopCopy: resolve(root, "apps/desktop/src/generated/peppyCopy.ts"),
  iosLogo: resolve(root, "apps/ios/PeppyMobile/Assets.xcassets/PeppyLogo.imageset/peppy-logo.svg"),
  iosLogoContents: resolve(root, "apps/ios/PeppyMobile/Assets.xcassets/PeppyLogo.imageset/Contents.json"),
  iosIconContents: resolve(root, "apps/ios/PeppyMobile/Assets.xcassets/AppIcon.appiconset/Contents.json"),
  favicon: resolve(root, "apps/desktop/public/favicon.svg"),
  coralLogo: resolve(root, "packages/desktop-ui/src/assets/peppy-logo-coral.svg"),
};
const coralFill = "#FF6B57";
const dpPerViewUnit = 160 / 512;
const iconCanvas = 1024;
const iconMarkScale = 1.6;
const launcherViewport = 108;
const launcherMarkScale = 0.125;
const iconDir = resolve(root, "apps/desktop/src-tauri/icons");
const iosIconDir = resolve(root, "apps/ios/PeppyMobile/Assets.xcassets/AppIcon.appiconset");
const defaultIconFiles = ["128x128.png", "128x128@2x.png", "icon.png", "icon.ico", "icon.icns"];
// iOS 26 uses one universal 1024px image per appearance; Xcode derives device sizes.
const iosDefaultIcons = ["AppIcon-512@2x.png"];

function extractTheme(css, selector) {
  const start = css.indexOf(selector);
  if (start < 0) throw new Error(`Missing ${selector}`);
  const body = css.slice(start, css.indexOf("}", start));
  return Object.fromEntries([...body.matchAll(/--peppy-([\w-]+):\s*([^;]+);/g)].map(([, key, value]) => [key, value.trim()]));
}
function parseColor(value) {
  const hex = value.replace(/^#/, "");
  if (/^[0-9a-f]{6}$/i.test(hex)) return [...hex.matchAll(/[0-9a-f]{2}/gi)].map(([channel]) => parseInt(channel, 16));
  if (/^[0-9a-f]{3}$/i.test(hex)) return [...hex].map(channel => parseInt(channel + channel, 16));
  const rgba = value.match(/^rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(0(?:\.\d+)?|1(?:\.0+)?)\s*\)$/);
  if (rgba) return rgba.slice(1, 4).map(Number).concat(Number(rgba[4]));
  throw new Error(`Expected hex or rgba token, received ${value}`);
}
function color(value, background) {
  const [red, green, blue, alpha = 1] = parseColor(value);
  const [backgroundRed, backgroundGreen, backgroundBlue] = parseColor(background);
  const channels = [red, green, blue].map((channel, index) => Math.round(channel * alpha + [backgroundRed, backgroundGreen, backgroundBlue][index] * (1 - alpha)));
  return `FF${channels.map(channel => channel.toString(16).padStart(2, "0")).join("").toUpperCase()}`;
}
function selected(theme) {
  return Object.fromEntries(semanticTokens.map(key => {
    if (!theme[key]) throw new Error(`Missing --peppy-${key}`);
    return [key, color(theme[key], theme["surface-panel"])];
  }));
}
function kotlin(theme) {
  const fields = semanticTokens.map(key => `    val ${kotlinNames[key]}: Color,`).join("\n");
  const scheme = name => semanticTokens.map(key => `    ${kotlinNames[key]} = Color(0x${theme[name][key]}),`).join("\n");
  return `// Generated by packages/mobile-design/scripts/generate.mjs. Do not edit.
package dev.peppy.mobile.ui.theme

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

@Immutable
data class PeppyColorScheme(
${fields}
)

object PeppyTokens {
  val Dark = PeppyColorScheme(
${scheme("dark")}
  )
  val Light = PeppyColorScheme(
${scheme("light")}
  )
}
`;
}
function swift(theme) {
  const fields = semanticTokens.map(key => `    let ${swiftNames[key]}: Color`).join("\n");
  const scheme = name => semanticTokens.map(key => `      ${swiftNames[key]}: Color(argb: 0x${theme[name][key]})`).join(",\n");
  return `// Generated by packages/mobile-design/scripts/generate.mjs. Do not edit.
import SwiftUI

struct PeppyColorScheme {
${fields}
}

enum PeppyTokens {
  static let dark = PeppyColorScheme(
${scheme("dark")}
  )
  static let light = PeppyColorScheme(
${scheme("light")}
  )

  static func colors(for colorScheme: ColorScheme) -> PeppyColorScheme {
    colorScheme == .dark ? dark : light
  }
}

private extension Color {
  init(argb: UInt32) {
    self.init(.sRGB, red: Double((argb >> 16) & 0xff) / 255, green: Double((argb >> 8) & 0xff) / 255, blue: Double(argb & 0xff) / 255, opacity: Double((argb >> 24) & 0xff) / 255)
  }
}
`;
}
function androidResourceEscape(value) {
  return value
    .replace(/\\/g, "\\\\")        // Backslash must be escaped first
    .replace(/'/g, "\\'")          // Apostrophe to \'
    .replace(/"/g, '\\"');         // Double quote to \"
}
function xmlEscape(value) { return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"); }
function androidCopy() { return `<?xml version="1.0" encoding="utf-8"?>
<!-- Generated by packages/mobile-design/scripts/generate.mjs. Do not edit. -->
<resources>
${Object.entries(nativeCatalog).map(([key, value]) => `    <string name="peppy_${key}">${xmlEscape(androidResourceEscape(value))}</string>`).join("\n")}
</resources>
`; }
function iosCopy() {
  const strings = Object.fromEntries(Object.entries(nativeCatalog).map(([key, value]) => [`peppy.${key}`, { localizations: { en: { stringUnit: { state: "translated", value } } } }]));
  return `${JSON.stringify({ sourceLanguage: "en", strings, version: "1.0" }, null, 2)}\n`;
}
function desktopCopy() {
  const entries = Object.entries(desktopCatalog).map(([key, value]) => `  ${key}: ${JSON.stringify(value)},`).join("\n");
  return `// Generated by packages/mobile-design/scripts/generate.mjs. Do not edit.
export const peppyCopy = {
${entries}
} as const;

export type PeppyCopyKey = keyof typeof peppyCopy;
`;
}
function parseMark(mark) {
  const viewBox = mark.match(/viewBox="([^"]+)"/)?.[1];
  const [minX, minY, width, height] = viewBox?.trim().split(/\s+/).map(Number) ?? [];
  const pathData = mark.match(/<path[^>]*\bd="([^"]+)"/)?.[1].replace(/\s+/g, " ").trim();
  const fill = mark.match(/<path[^>]*\bfill="(#[0-9A-Fa-f]{6})"/)?.[1];
  if (minX !== 0 || minY !== 0 || !(width > 0) || !(height > 0) || !pathData || !fill) throw new Error("Canonical mark is missing its geometry or fill");
  if ((mark.match(/<path\b/g) ?? []).length !== 1 || /\btransform=/.test(mark)) throw new Error("Canonical mark must be one untransformed path");
  return { fill, pathData, viewBox, width, height };
}
function round(value) { return Number(value.toFixed(6)); }
function androidVector(mark, tint = mark.fill) {
  return `<?xml version="1.0" encoding="utf-8"?>
<!-- Generated from apps/desktop/public/peppy-logo.svg. -->
<vector xmlns:android="http://schemas.android.com/apk/res/android" android:width="${round(mark.width * dpPerViewUnit)}dp" android:height="${round(mark.height * dpPerViewUnit)}dp" android:viewportWidth="${mark.width}" android:viewportHeight="${mark.height}">
    <path android:fillColor="${tint}" android:pathData="${mark.pathData}"/>
</vector>
`;
}
function launcherForeground(mark, tint = mark.fill) {
  const center = launcherViewport / 2;
  return `<?xml version="1.0" encoding="utf-8"?>
<!-- Generated from the canonical mark inside Android's 66dp circular safe zone. -->
<vector xmlns:android="http://schemas.android.com/apk/res/android" android:width="${launcherViewport}dp" android:height="${launcherViewport}dp" android:viewportWidth="${launcherViewport}" android:viewportHeight="${launcherViewport}">
    <group android:translateX="${round(center - mark.width * launcherMarkScale / 2)}" android:translateY="${round(center - mark.height * launcherMarkScale / 2)}" android:scaleX="${launcherMarkScale}" android:scaleY="${launcherMarkScale}">
        <path android:fillColor="${tint}" android:pathData="${mark.pathData}"/>
    </group>
</vector>
`;
}
function adaptive() {
  return `<?xml version="1.0" encoding="utf-8"?>
<!-- Generated from the canonical Peppy mark. -->
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@drawable/ic_launcher_background"/>
    <foreground android:drawable="@drawable/ic_launcher_foreground"/>
    <monochrome android:drawable="@drawable/ic_launcher_monochrome"/>
</adaptive-icon>
`;
}
function squareIcon(mark, background, foreground) {
  const x = round((iconCanvas - mark.width * iconMarkScale) / 2);
  const y = round((iconCanvas - mark.height * iconMarkScale) / 2);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${iconCanvas}" height="${iconCanvas}" viewBox="0 0 ${iconCanvas} ${iconCanvas}">
  <rect width="${iconCanvas}" height="${iconCanvas}" fill="${background}"/>
  <g transform="translate(${x} ${y}) scale(${iconMarkScale})">
    <path fill="${foreground}" d="${mark.pathData}"/>
  </g>
</svg>
`;
}
function transparentIcon(mark) {
  const x = round((iconCanvas - mark.width * iconMarkScale) / 2);
  const y = round((iconCanvas - mark.height * iconMarkScale) / 2);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${iconCanvas}" height="${iconCanvas}" viewBox="0 0 ${iconCanvas} ${iconCanvas}">
  <g transform="translate(${x} ${y}) scale(${iconMarkScale})">
    <path fill="${mark.fill}" d="${mark.pathData}"/>
  </g>
</svg>
`;
}
function iosLogoContents() { return `{
  "images" : [ { "filename" : "peppy-logo.svg", "idiom" : "universal", "scale" : "1x" } ],
  "info" : { "author" : "xcode", "version" : 1 }
}
`; }
function iosRootContents() { return `{
  "info" : { "author" : "xcode", "version" : 1 }
}
`; }
function variantName(filename, appearance) { return filename.replace(/\.png$/, `-${appearance}.png`); }
function iosIconContents() {
  const filename = iosDefaultIcons[0];
  const image = (appearance) => ({
    filename: appearance ? variantName(filename, appearance) : filename,
    idiom: "universal", platform: "ios", size: "1024x1024",
    ...(appearance ? { appearances: [{ appearance: "luminosity", value: appearance }] } : {}),
  });
  return `${JSON.stringify({ images: [image(), image("dark"), image("tinted")], info: { author: "xcode", version: 1 } }, null, 2)}\n`;
}

async function outputs() {
  const [css, sourceMark] = await Promise.all([readFile(paths.css, "utf8"), readFile(paths.mark, "utf8")]);
  const mark = parseMark(sourceMark);
  const theme = { dark: selected(extractTheme(css, ":root,")), light: selected(extractTheme(css, ".theme-light {")) };
  const icon = squareIcon(mark, "#FFFFFF", mark.fill);
  return new Map([
    [paths.appIcon, icon], [paths.windowsAppIcon, transparentIcon(mark)], [paths.darkAppIcon, squareIcon(mark, "#1A1A1A", theme.dark.brand.replace("FF", "#"))], [paths.tintedAppIcon, squareIcon(mark, "#FFFFFF", "#000000")],
    [paths.androidTokens, kotlin(theme)], [paths.androidStrings, androidCopy()], [paths.androidLogo, androidVector(mark)],
    [paths.androidForeground, launcherForeground(mark)], [paths.androidBackground, `<?xml version="1.0" encoding="utf-8"?>\n<shape xmlns:android="http://schemas.android.com/apk/res/android" android:shape="rectangle"><solid android:color="#FFFFFFFF"/></shape>\n`],
    [paths.androidMonochrome, launcherForeground(mark, "#FF000000")], [paths.androidIcon, adaptive()], [paths.androidRoundIcon, adaptive()],
    [paths.iosRoot, iosRootContents()], [paths.iosTokens, swift(theme)], [paths.iosStrings, iosCopy()], [paths.iosLogo, sourceMark], [paths.iosLogoContents, iosLogoContents()], [paths.iosIconContents, iosIconContents()],
    [paths.desktopCopy, desktopCopy()],
    [paths.favicon, sourceMark], [paths.coralLogo, sourceMark.replace(mark.fill, coralFill)],
  ]);
}
async function writeOutputs(output) { await Promise.all([...output].map(async ([path, content]) => { await mkdir(dirname(path), { recursive: true }); await writeFile(path, content, "utf8"); })); }
function pngSize(path) { const png = readFileSync(path); return png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])) ? [png.readUInt32BE(16), png.readUInt32BE(20)] : null; }
function verifyIcons() {
  const iosIcons = iosDefaultIcons.flatMap(file => [file, variantName(file, "dark"), variantName(file, "tinted")]);
  for (const file of [...defaultIconFiles.map(file => resolve(iconDir, file)), ...iosIcons.map(file => resolve(iosIconDir, file)), resolve(root, "apps/desktop/public/favicon-32.png")]) if (!existsSync(file)) throw new Error(`Missing generated icon: ${file}`);
  for (const file of [resolve(iconDir, "icon.png"), resolve(iosIconDir, "AppIcon-512@2x.png"), resolve(iosIconDir, variantName("AppIcon-512@2x.png", "dark")), resolve(iosIconDir, variantName("AppIcon-512@2x.png", "tinted"))]) if (pngSize(file)?.join("x") !== (file.includes("icon.png") ? "512x512" : "1024x1024")) throw new Error(`Unexpected icon dimensions: ${file}`);
  verifyIcoTransparency(resolve(iconDir, "icon.ico"));
}
async function withIconOutput(action) {
  const output = await mkdtemp(join(tmpdir(), "peppy-icons-"));
  try {
    await action(output);
  } finally {
    await rm(output, { recursive: true, force: true });
  }
}
function tauriIcon(source, output) {
  execFileSync("pnpm", ["--dir", "apps/desktop", "exec", "tauri", "icon", source, "--output", output, "--fit", "contain", "--ios-color", "#FFFFFF"], { cwd: root, stdio: "inherit" });
}
async function generateWindowsIcon() {
  await withIconOutput(async output => {
    tauriIcon(paths.windowsAppIcon, output);
    await cp(resolve(output, "icon.ico"), resolve(iconDir, "icon.ico"));
  });
}
async function generateIcons() {
  await withIconOutput(async output => {
    tauriIcon(paths.appIcon, output);
    await Promise.all(["128x128.png", "128x128@2x.png", "icon.png"].map(file => cp(resolve(output, file), resolve(iconDir, file))));
    await Promise.all(iosDefaultIcons.map(file => cp(resolve(output, "ios", file), resolve(iosIconDir, file))));
    for (const file of iosDefaultIcons) {
      const [width, height] = pngSize(resolve(iosIconDir, file));
      for (const [appearance, source] of [["dark", paths.darkAppIcon], ["tinted", paths.tintedAppIcon]]) {
        execFileSync("sips", ["-z", String(height), String(width), "-s", "format", "png", source, "--out", resolve(iosIconDir, variantName(file, appearance))], { stdio: "inherit" });
      }
    }
    const current = new Set(iosDefaultIcons.flatMap(file => [file, variantName(file, "dark"), variantName(file, "tinted")]));
    for (const file of await readdir(iosIconDir)) {
      if (file.startsWith("AppIcon-") && file.endsWith(".png") && !current.has(file)) {
        await rm(resolve(iosIconDir, file));
      }
    }
    await cp(resolve(output, "32x32.png"), resolve(root, "apps/desktop/public/favicon-32.png"));
  });
  await generateWindowsIcon();
}
async function main() {
  const output = await outputs();
  if (process.argv.includes("--check")) {
    const stale = [...output].filter(([path, content]) => !existsSync(path) || readFileSync(path, "utf8") !== content).map(([path]) => path);
    if (stale.length) throw new Error(`Generated mobile design files are stale:\n${stale.join("\n")}`);
    verifyIcons();
    return;
  }
  await writeOutputs(output);
  if (process.argv.includes("--icons")) await generateIcons();
  else if (process.argv.includes("--windows-icon")) await generateWindowsIcon();
}
await main();
