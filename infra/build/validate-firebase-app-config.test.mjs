import assert from "node:assert/strict";
import { access, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { firebaseAppConfigEnvironmentVariable, validateFirebaseAppConfig, writeFirebaseAppConfigFromEnvironment } from "./validate-firebase-app-config.mjs";

const validConfig = {
  project_info: { project_id: "peppy-mobile" },
  client: [{
    client_info: {
      mobilesdk_app_id: "1:1234567890:android:abcdef",
      android_client_info: { package_name: "dev.peppy.mobile" },
    },
    api_key: [{ current_key: "public-api-key" }],
  }],
};

function configWith(change) {
  const config = structuredClone(validConfig);
  change(config);
  return JSON.stringify(config);
}

test("accepts a complete public Firebase Android app configuration", () => {
  assert.deepEqual(validateFirebaseAppConfig(JSON.stringify(validConfig)), validConfig);
});

test("rejects malformed Firebase app configuration JSON without echoing input", () => {
  assert.throws(() => validateFirebaseAppConfig('{"private_key":"pasted-secret"'), {
    message: "Firebase app configuration must be valid JSON",
  });
});

test("rejects secret and service-account fields", () => {
  assert.throws(() => validateFirebaseAppConfig(configWith(config => { config.private_key = "secret"; })), /must not contain secret or service-account fields/);
  assert.throws(() => validateFirebaseAppConfig(configWith(config => { config.service_account = {}; })), /must not contain secret or service-account fields/);
});

test("rejects missing or incorrect Android package clients", () => {
  assert.throws(() => validateFirebaseAppConfig(configWith(config => { config.client = []; })), /must include Android package dev\.peppy\.mobile/);
  assert.throws(() => validateFirebaseAppConfig(configWith(config => { config.client[0].client_info.android_client_info.package_name = "example.invalid"; })), /must include Android package dev\.peppy\.mobile/);
});

test("rejects incomplete Firebase app configurations", () => {
  assert.throws(() => validateFirebaseAppConfig(configWith(config => { delete config.project_info.project_id; })), /must include project_info\.project_id/);
  assert.throws(() => validateFirebaseAppConfig(configWith(config => { delete config.client[0].client_info.mobilesdk_app_id; })), /must include client_info\.mobilesdk_app_id/);
  assert.throws(() => validateFirebaseAppConfig(configWith(config => { config.client[0].api_key = []; })), /must include a nonempty api_key/);
});

test("writes a validated configuration supplied through the environment", async () => {
  const directory = await mkdtemp(join(tmpdir(), "peppy-firebase-config-"));
  const outputPath = join(directory, "google-services.json");
  try {
    const environment = { [firebaseAppConfigEnvironmentVariable]: JSON.stringify(validConfig) };
    assert.equal(await writeFirebaseAppConfigFromEnvironment({ environment, outputPath }), true);
    assert.deepEqual(JSON.parse(await readFile(outputPath, "utf8")), validConfig);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("leaves the self-hosting configuration path untouched when app configuration is absent", async () => {
  const directory = await mkdtemp(join(tmpdir(), "peppy-firebase-config-"));
  const outputPath = join(directory, "google-services.json");
  try {
    assert.equal(await writeFirebaseAppConfigFromEnvironment({ environment: {}, outputPath }), false);
    await assert.rejects(access(outputPath));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
