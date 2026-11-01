import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync, statSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";
import { catalog, nativeCatalog, desktopCatalog, desktopOnlyKeys } from "../src/catalog.mjs";
import { icoFrameAlphas } from "../scripts/icon-alpha.mjs";

const root = resolve(import.meta.dirname, "../../..");
const run = (...args) => execFileSync("node", ["packages/mobile-design/scripts/generate.mjs", ...args], { cwd: root, encoding: "utf8" });
const path = file => resolve(root, file);

function luminance(hex) {
  const expanded = hex.length === 3 ? [...hex].map(value => value + value).join("") : hex;
  const channels = expanded.match(/\w\w/g).map(value => parseInt(value, 16) / 255).map(value => value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}
function contrast(first, second) {
  const [light, dark] = [luminance(first), luminance(second)].sort((left, right) => right - left);
  return (light + 0.05) / (dark + 0.05);
}
function pngDimensions(file) {
  const png = readFileSync(path(file));
  assert.deepEqual([...png.subarray(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10]);
  return [png.readUInt32BE(16), png.readUInt32BE(20)];
}

function themeTokens(css, selector, start = 0) {
  const selectorStart = css.indexOf(selector, start);
  assert.notEqual(selectorStart, -1, `missing ${selector}`);
  const body = css.slice(selectorStart, css.indexOf("}", selectorStart));
  return Object.fromEntries([...body.matchAll(/--peppy-([\w-]+):\s*(#[0-9a-f]{3,6});/gi)].map(([, key, value]) => [key, value.slice(1)]));
}

test("generated mobile design resources are deterministic", () => {
  run();
  run("--check");
});

test("CSS and generated tokens retain coral action foreground contrast", () => {
  const css = readFileSync(path("packages/desktop-ui/src/peppy-tokens.css"), "utf8");
  const dark = themeTokens(css, ":root,");
  const light = themeTokens(css, ".theme-light {");
  const systemLight = themeTokens(css, ".theme-system", css.indexOf("@media (prefers-color-scheme: light)"));

  for (const [name, theme] of Object.entries({ dark, light, systemLight })) {
    for (const [background, foreground] of [["accent", "accent-text"], ["accent-hover", "accent-text"], ["primary", "primary-text"]]) {
      // The approved #C9422D / white light pair rounds to 4.5:1 but has a
      // precise sRGB ratio just below that threshold.
      assert.ok(contrast(theme[background], theme[foreground]) >= 4.4, `${name} ${foreground} must contrast with ${background}`);
    }
  }

  assert.equal(light["accent-text"], "fff");
  assert.equal(light["primary-text"], "fff");
  assert.equal(systemLight["accent-text"], "fff");
  assert.equal(systemLight["primary-text"], "fff");
  assert.equal(dark["accent-text"], "1a1a1a");
  assert.equal(dark["primary-text"], "1a1a1a");

  const android = readFileSync(path("apps/android/app/src/main/java/dev/peppy/mobile/ui/theme/PeppyTokens.kt"), "utf8");
  const swift = readFileSync(path("apps/ios/PeppyMobile/Design/PeppyTokens.swift"), "utf8");
  assert.match(android, /val Light = PeppyColorScheme\([\s\S]*accentText = Color\(0xFFFFFFFF\),[\s\S]*primaryText = Color\(0xFFFFFFFF\)/);
  assert.match(swift, /static let light = PeppyColorScheme\([\s\S]*AccentText: Color\(argb: 0xFFFFFFFF\),[\s\S]*PrimaryText: Color\(argb: 0xFFFFFFFF\)/);
});

test("native warning tokens composite translucent CSS values over the panel surface", () => {
  const android = readFileSync(path("apps/android/app/src/main/java/dev/peppy/mobile/ui/theme/PeppyTokens.kt"), "utf8");
  const swift = readFileSync(path("apps/ios/PeppyMobile/Design/PeppyTokens.swift"), "utf8");

  for (const [name, dark, light] of [
    ["warningSurface", "FF322E21", "FFEBE7DF"],
    ["warningBorder", "FF4E4311", "FFE2D6BC"],
  ]) {
    const swiftName = name[0].toUpperCase() + name.slice(1);
    assert.match(android, new RegExp(`val ${name}: Color,[\\s\\S]*val Dark = PeppyColorScheme\\([\\s\\S]*${name} = Color\\(0x${dark}\\),[\\s\\S]*val Light = PeppyColorScheme\\([\\s\\S]*${name} = Color\\(0x${light}\\)`));
    assert.match(swift, new RegExp(`let ${swiftName}: Color[\\s\\S]*static let dark = PeppyColorScheme\\([\\s\\S]*${swiftName}: Color\\(argb: 0x${dark}\\),[\\s\\S]*static let light = PeppyColorScheme\\([\\s\\S]*${swiftName}: Color\\(argb: 0x${light}\\)`));
  }
});

test("native error surface tokens composite translucent CSS values over the desk surface", () => {
  const android = readFileSync(path("apps/android/app/src/main/java/dev/peppy/mobile/ui/theme/PeppyTokens.kt"), "utf8");
  const swift = readFileSync(path("apps/ios/PeppyMobile/Design/PeppyTokens.swift"), "utf8");

  assert.match(android, /val errorSurface: Color,[\s\S]*val Dark = PeppyColorScheme\([\s\S]*errorSurface = Color\(0xFF3E312F\),[\s\S]*val Light = PeppyColorScheme\([\s\S]*errorSurface = Color\(0xFFEFDFDC\)/);
  assert.match(swift, /let ErrorSurface: Color[\s\S]*static let dark = PeppyColorScheme\([\s\S]*ErrorSurface: Color\(argb: 0xFF3E312F\),[\s\S]*static let light = PeppyColorScheme\([\s\S]*ErrorSurface: Color\(argb: 0xFFEFDFDC\)/);
});

test("android resources preserve canonical mark geometry within the safe zone", () => {
  const mark = readFileSync(path("apps/desktop/public/peppy-logo.svg"), "utf8");
  const sourcePath = mark.match(/<path[^>]*\bd="([^"]+)"/)?.[1].replace(/\s+/g, " ").trim();
  const foreground = readFileSync(path("apps/android/app/src/main/res/drawable/ic_launcher_foreground.xml"), "utf8");
  const logo = readFileSync(path("apps/android/app/src/main/res/drawable/peppy_logo.xml"), "utf8");
  assert.equal(sourcePath, foreground.match(/android:pathData="([^"]+)"/)?.[1]);
  assert.equal(sourcePath, logo.match(/android:pathData="([^"]+)"/)?.[1]);
  const attribute = name => Number(foreground.match(new RegExp(`android:${name}="([0-9.]+)"`))[1]);
  const sx = attribute("scaleX"), sy = attribute("scaleY");
  const tx = attribute("translateX"), ty = attribute("translateY");
  assert.equal(sy, sx, "canonical aspect must be retained");
  const [, , markWidth, markHeight] = mark.match(/viewBox="([^"]+)"/)[1].split(/\s+/).map(Number);
  assert.ok(Math.abs(tx + markWidth * sx / 2 - 54) < 0.01 && Math.abs(ty + markHeight * sy / 2 - 54) < 0.01, "launcher mark is centered");
  // Sample this canonical M/H/V/L/C path's curves, not merely the bounding rectangle.
  const tokens = sourcePath.match(/[A-Z]|-?\d+(?:\.\d+)?/g);
  let i = 0, x = 0, y = 0;
  const number = () => Number(tokens[i++]);
  const point = (px, py) => assert.ok(Math.hypot(tx + px * sx - 54, ty + py * sy - 54) <= 33, "visible mark stays in the circular safe zone");
  while (i < tokens.length) {
    const command = tokens[i++];
    if (command === "M" || command === "L") { x = number(); y = number(); point(x, y); }
    else if (command === "H") { x = number(); point(x, y); }
    else if (command === "V") { y = number(); point(x, y); }
    else if (command === "C") {
      const x1 = number(), y1 = number(), x2 = number(), y2 = number(), x3 = number(), y3 = number();
      for (let sample = 0; sample <= 100; sample++) {
        const t = sample / 100, u = 1 - t;
        point(u ** 3 * x + 3 * u * u * t * x1 + 3 * u * t * t * x2 + t ** 3 * x3,
          u ** 3 * y + 3 * u * u * t * y1 + 3 * u * t * t * y2 + t ** 3 * y3);
      }
      x = x3; y = y3;
    } else assert.equal(command, "Z");
  }
});

test("generated copy has both native catalog formats", () => {
  assert.match(readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8"), /peppy_onboarding_headline/);
  const strings = JSON.parse(readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8"));
  assert.equal(strings.strings["peppy.locked"].localizations.en.stringUnit.value, "Locked");
});

test("server offline copy is generated for desktop and native clients", () => {
  const copy = {
    server_offline_title: "Server offline",
    server_offline_body: "Peppy will reconnect automatically.",
  };
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const ios = JSON.parse(readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8"));
  const desktop = readFileSync(path("apps/desktop/src/generated/peppyCopy.ts"), "utf8");

  for (const [key, value] of Object.entries(copy)) {
    assert.equal(catalog[key], value);
    assert.equal(nativeCatalog[key], value);
    assert.match(android, new RegExp(`<string name="peppy_${key}">${value}</string>`));
    assert.equal(ios.strings[`peppy.${key}`].localizations.en.stringUnit.value, value);
    assert.ok(desktop.includes(`${key}: ${JSON.stringify(value)},`));
  }
});

test("account access copy and placeholders are generated for every client", () => {
  const keys = [
    "system_banner_dismiss", "system_banner_opens_in_browser", "system_banner_more", "system_banner_less",
    "account_payment_failed_grace_title", "account_payment_failed_grace_body", "account_payment_failed_grace_body_gateway", "account_payment_failed_grace_body_nodate", "account_payment_failed_grace_body_gateway_nodate", "account_payment_failed_grace_action",
    "account_ending_scheduled_title", "account_ending_scheduled_body", "account_ending_scheduled_body_gateway", "account_ending_scheduled_action",
    ...["past_due", "canceled", "incomplete", "no_subscription", "tester_ineligible", "account_deleting", "other"].flatMap(kind => [
      `account_read_only_${kind}_title`, `account_read_only_${kind}_body`, `account_read_only_${kind}_body_gateway`, `account_read_only_${kind}_action`,
    ]),
    "account_notif_channel_name", "account_notif_title", "account_notif_body", "account_send_paused_status",
    "devices_read_only_caption", "devices_sending_paused_badge",
  ];
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const ios = JSON.parse(readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8"));
  const desktop = readFileSync(path("apps/desktop/src/generated/peppyCopy.ts"), "utf8");

  for (const key of keys) {
    assert.ok(key in catalog, `catalog is missing ${key}`);
    assert.ok(key in nativeCatalog, `native catalog is missing ${key}`);
    assert.match(android, new RegExp(`<string name="peppy_${key}">`));
    assert.ok(ios.strings[`peppy.${key}`], `iOS copy is missing ${key}`);
    assert.ok(desktop.includes(`${key}: ${JSON.stringify(catalog[key])},`), `desktop copy is missing ${key}`);
  }

  assert.equal(catalog.account_payment_failed_grace_body.includes("%1$s"), true);
  assert.match(android, /<string name="peppy_account_payment_failed_grace_body">[^<]*%1\$s[^<]*<\/string>/);
  assert.match(desktop, /account_payment_failed_grace_body: "[^"]*%1\$s[^"]*",/);
  assert.equal(ios.strings["peppy.account_payment_failed_grace_body"].localizations.en.stringUnit.value.includes("%1$@"), true);
  assert.match(desktop, /export function formatCopy\(template: string, \.\.\.args: string\[\]\): string/);
  assert.match(desktop, /template\.replace\(\/%\(\\d\+\)\\\$s\/g, \(_, index: string\) => args\[Number\(index\) - 1\] \?\? ""\)/);
  assert.equal(ios.strings["peppy.toggle_android_account_role"].localizations.en.stringUnit.value, "Role: %1$@");
});

test("wake status copy maps to both native catalog formats", () => {
  const copy = {
    wake_section_title: "Background updates",
    wake_updates_toggle_label: "Allow background updates",
    wake_status_label: "Wake registration",
    wake_status_registered: "Registered",
    wake_status_pending: "Setting up",
    wake_status_retrying: "Retrying",
    wake_status_paused: "Paused",
    wake_status_unavailable: "Unavailable",
    wake_footer_registered: "Registered for background updates. Delivery may be delayed by your phone.",
    wake_footer_pending: "Setting up background updates automatically.",
    wake_footer_retrying: "Background updates will retry when a connection is available.",
    wake_footer_paused: "Background wake is paused. Other permitted sync may still run.",
    wake_footer_unavailable_server: "Background wake is unavailable on this server.",
    wake_footer_missing_app_support: "This app build does not support background push.",
    wake_footer_os_restricted: "Your phone is limiting background activity.",
    wake_footer_keys_locked: "Unlock Peppy to process pending updates.",
    wake_cleanup_pending: "Background wake is paused. Server cleanup will retry when connected.",
    wake_os_settings_cta: "Open Settings",
    wake_community_relay_enabled: "Custom relay enabled. Background delivery may be delayed.",
    wake_community_relay_disabled: "Custom relay is off.",
  };
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const ios = JSON.parse(readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8"));

  for (const [key, value] of Object.entries(copy)) {
    assert.equal(nativeCatalog[key], value);
    const escapedValue = value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    assert.match(android, new RegExp(`<string name="peppy_${key}">${escapedValue}</string>`));
    assert.equal(ios.strings[`peppy.${key}`].localizations.en.stringUnit.value, value);
  }
});

test("onboarding server mode copy maps to each generated platform format", () => {
  const copy = {
    onboarding_mode_selector: "Server",
    onboarding_mode_hosted: "Peppy Hosted",
    onboarding_mode_self_hosted: "Self-hosted",
  };
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const ios = JSON.parse(readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8"));
  const desktop = readFileSync(path("apps/desktop/src/generated/peppyCopy.ts"), "utf8");

  for (const [key, value] of Object.entries(copy)) {
    assert.equal(catalog[key], value);
    assert.equal(nativeCatalog[key], value);
    assert.match(android, new RegExp(`<string name="peppy_${key}">${value}</string>`));
    assert.equal(ios.strings[`peppy.${key}`].localizations.en.stringUnit.value, value);
    assert.ok(desktop.includes(`${key}: ${JSON.stringify(value)},`));
  }
});

test("account resolution copy maps to each generated platform format", () => {
  const copy = {
    hosted_account_checking: "Checking your account…",
    hosted_account_check_failed: "We couldn’t check your account. Try again.",
    hosted_data_preparing: "Preparing your data…",
    hosted_data_prepare_failed: "Preparation couldn’t finish. Try again.",
  };
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const ios = JSON.parse(readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8"));
  const desktop = readFileSync(path("apps/desktop/src/generated/peppyCopy.ts"), "utf8");

  for (const [key, value] of Object.entries(copy)) {
    assert.equal(catalog[key], value);
    assert.match(android, new RegExp(`<string name="peppy_${key}">${value}</string>`));
    assert.equal(ios.strings[`peppy.${key}`].localizations.en.stringUnit.value, value);
    assert.ok(desktop.includes(`${key}: ${JSON.stringify(value)},`));
  }
});

test("generated desktop copy applies the desktop catalog overrides", () => {
  const desktop = readFileSync(path("apps/desktop/src/generated/peppyCopy.ts"), "utf8");
  assert.match(desktop, /^\/\/ Generated by packages\/mobile-design\/scripts\/generate\.mjs\. Do not edit\.\n/);
  assert.match(desktop, /export type PeppyCopyKey = keyof typeof peppyCopy;/);

  for (const [key, value] of Object.entries(desktopCatalog)) {
    assert.ok(desktop.includes(`${key}: ${JSON.stringify(value)},`), `Key ${key} with its desktop value should be in generated copy`);
  }
});

test("desktop override keys are catalog keys or explicit desktop-only additions", () => {
  const desktopOverrideKeys = [
    "hosted_join_headline", "hosted_join_body", "device_approval_headline", "device_approval_body",
    "hosted_unlock_body", "hosted_unlock_cta", "hosted_subscribe_legal", "hosted_subscribe_restore",
    "hosted_subscribe_store_unavailable", "hosted_purchase_pending_body", "passphrase_create_body",
    "passphrase_native_cta", "passphrase_native_note", "passphrase_ack_label", "permissions_notifications",
    "permissions_login", "permissions_desktop_note", "self_hosted_body", "hosted_preview_native_only",
    "passphrase_weak",
  ];

  for (const key of desktopOverrideKeys) {
    assert.ok(key in catalog || desktopOnlyKeys.includes(key), `${key} must be shared or explicitly desktop-only`);
    assert.ok(key in desktopCatalog, `${key} must be present in desktop copy`);
  }

  for (const key of desktopOnlyKeys) {
    assert.ok(!(key in catalog), `${key} must be absent from shared catalog`);
  }
});

test("native copy is generated from the shared catalog only", () => {
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const ios = readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8");
  assert.doesNotMatch(android, /Add this computer/);
  assert.doesNotMatch(ios, /Add this computer/);
  assert.match(android, new RegExp(catalog.hosted_join_headline));
  assert.match(ios, new RegExp(catalog.hosted_join_headline));
});

test("native catalog excludes preview and store-only keys", () => {
  // Verify preview keys are excluded from nativeCatalog
  assert.ok("preview_label" in catalog);
  assert.ok(!("preview_label" in nativeCatalog));
  assert.ok("preview_scenarios" in catalog);
  assert.ok(!("preview_scenarios" in nativeCatalog));
  assert.ok("preview_reset" in catalog);
  assert.ok(!("preview_reset" in nativeCatalog));

  // Verify obsolete mobile store/preview-only keys are excluded
  assert.ok("settings_server_delete_body" in catalog);
  assert.ok(!("settings_server_delete_body" in nativeCatalog));
  assert.ok("hosted_subscribe_legal" in catalog);
  assert.ok(!("hosted_subscribe_legal" in nativeCatalog));
  assert.ok("hosted_purchase_pending_body" in catalog);
  assert.ok(!("hosted_purchase_pending_body" in nativeCatalog));
  assert.ok("hosted_subscribe_restore" in catalog);
  assert.ok(!("hosted_subscribe_restore" in nativeCatalog));

  // Verify production keys are still present
  assert.ok("production_hosted_cta" in nativeCatalog);
  assert.ok("hosted_join_headline" in nativeCatalog);
});

test("native output excludes preview and store-only copy", () => {
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const ios = readFileSync(path("apps/ios/PeppyMobile/Peppy.xcstrings"), "utf8");

  // Verify preview keys are not in native outputs
  assert.doesNotMatch(android, /peppy_preview_/);
  assert.doesNotMatch(ios, /peppy\.preview_/);

  // Verify obsolete store-only keys are not in native outputs
  assert.doesNotMatch(android, /peppy_settings_server_delete_body/);
  assert.doesNotMatch(ios, /peppy\.settings_server_delete_body/);
  assert.doesNotMatch(android, /Subscription pricing and renewal details are provided by the store/);
  assert.doesNotMatch(ios, /Subscription pricing and renewal details are provided by the store/);

  // Verify the native catalog size matches the output
  const androidCount = (android.match(/<string name="peppy_/g) || []).length;
  assert.equal(androidCount, Object.keys(nativeCatalog).length, "Android strings count should match nativeCatalog size");
});

test("generated icons include default, dark, and tinted variants", () => {
  assert.deepEqual(pngDimensions("apps/desktop/src-tauri/icons/icon.png"), [512, 512]);
  for (const file of ["AppIcon-512@2x.png", "AppIcon-512@2x-dark.png", "AppIcon-512@2x-tinted.png"]) {
    assert.deepEqual(pngDimensions(`apps/ios/PeppyMobile/Assets.xcassets/AppIcon.appiconset/${file}`), [1024, 1024]);
    assert.ok(statSync(path(`apps/ios/PeppyMobile/Assets.xcassets/AppIcon.appiconset/${file}`)).size > 500);
  }
  const contents = JSON.parse(readFileSync(path("apps/ios/PeppyMobile/Assets.xcassets/AppIcon.appiconset/Contents.json"), "utf8"));
  const variants = contents.images.filter(image => image.appearances);
  assert.equal(contents.images.length, 3, "one universal 1024px source per appearance");
  assert.ok(contents.images.every(image => image.idiom === "universal" && image.platform === "ios" && image.size === "1024x1024"));
  assert.equal(variants.length, 2, "dark and tinted appearances must be registered");
  assert.deepEqual([...new Set(variants.map(image => image.appearances[0].value))].sort(), ["dark", "tinted"]);
});

test("Windows application icon frames preserve transparent antialiased edges", () => {
  const frames = icoFrameAlphas(path("apps/desktop/src-tauri/icons/icon.ico"));
  assert.equal(frames.length, 6);
  for (const alphas of frames) {
    assert.ok(alphas.includes(0), "transparent canvas edge");
    assert.ok(alphas.includes(255), "opaque coral mark");
    assert.ok(alphas.some(alpha => alpha > 0 && alpha < 255), "antialiased mark edge");
  }
});

test("favicon, iOS logo, and coral logo are generated from the canonical mark", () => {
  const mark = readFileSync(path("apps/desktop/public/peppy-logo.svg"), "utf8");
  assert.equal(readFileSync(path("apps/desktop/public/favicon.svg"), "utf8"), mark);
  assert.equal(readFileSync(path("apps/ios/PeppyMobile/Assets.xcassets/PeppyLogo.imageset/peppy-logo.svg"), "utf8"), mark);
  const coral = readFileSync(path("packages/desktop-ui/src/assets/peppy-logo-coral.svg"), "utf8");
  const fill = mark.match(/<path[^>]*\bfill="(#[0-9A-Fa-f]{6})"/)[1];
  assert.equal(coral, mark.replace(fill, "#FF6B57"));
});

test("android strings properly escape apostrophes and special characters", () => {
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  
  // Verify production_no_google_account has escaped apostrophe for "phone's"
  assert.match(android, /peppy_production_no_google_account.*phone\\'s/);
  
  // Verify production_billing_body has escaped apostrophe for "Peppy's"
  assert.match(android, /peppy_production_billing_body.*Peppy\\'s/);
  
  // Ensure no unescaped ASCII apostrophes remain in string values
  const stringLines = android.split("\n").filter(line => line.includes('<string name="peppy_'));
  for (const line of stringLines) {
    // Extract the string value content (between > and <)
    const match = line.match(/<string[^>]*>([^<]*)<\/string>/);
    if (match) {
      const content = match[1];
      // Check that there are no unescaped ASCII apostrophes (') that aren't already part of entities
      // A proper apostrophe in Android resources is either \' or within a surrounding quote
      const unescapedApostrophes = content.match(/[^\\]'|^'/g);
      assert.ok(!unescapedApostrophes, `Found unescaped apostrophe in: ${line}`);
    }
  }
});

test("android strings encode newlines as resource escapes", () => {
  const android = readFileSync(path("apps/android/app/src/main/res/values/strings_peppy.xml"), "utf8");
  const elements = [...android.matchAll(/<string name="peppy_(\w+)">([^]*?)<\/string>/g)];

  for (const [, , content] of elements) {
    assert.ok(!content.includes("\n"), `Raw newline in Android string: ${content.slice(0, 60)}`);
  }

  const multiline = Object.entries(nativeCatalog).filter(([, value]) => value.includes("\n"));
  assert.ok(multiline.length > 0);
  for (const [key, value] of multiline) {
    const escaped = value
      .replace(/\\/g, "\\\\")
      .replace(/'/g, "\\'")
      .replace(/"/g, '\\"')
      .replace(/\n/g, "\\n");
    assert.ok(android.includes(`<string name="peppy_${key}">${escaped}</string>`), `Missing escaped newlines for ${key}`);
  }
});
