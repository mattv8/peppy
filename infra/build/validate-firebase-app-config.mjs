import { writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

export const firebaseAppConfigEnvironmentVariable = "PEPPY_ANDROID_FIREBASE_APP_CONFIG";
export const firebaseAppConfigPath = "apps/android/app/google-services.json";
export const androidPackageName = "dev.peppy.mobile";

function isNonEmptyString(value) {
  return typeof value === "string" && value.trim().length > 0;
}

function containsPrivateCredentials(value) {
  if (Array.isArray(value)) return value.some(containsPrivateCredentials);
  if (!value || typeof value !== "object") return false;

  return Object.entries(value).some(([key, nestedValue]) => {
    const normalizedKey = key.toLowerCase().replace(/[_-]/g, "");
    return normalizedKey.includes("secret") || normalizedKey.includes("privatekey") || normalizedKey.includes("serviceaccount") || containsPrivateCredentials(nestedValue);
  });
}

export function validateFirebaseAppConfig(rawConfig) {
  let config;
  try {
    config = JSON.parse(rawConfig);
  } catch {
    throw new Error("Firebase app configuration must be valid JSON");
  }

  if (!config || typeof config !== "object" || Array.isArray(config)) {
    throw new Error("Firebase app configuration must be a JSON object");
  }
  if (containsPrivateCredentials(config)) {
    throw new Error("Firebase app configuration must not contain secret or service-account fields");
  }
  if (!isNonEmptyString(config.project_info?.project_id)) {
    throw new Error("Firebase app configuration must include project_info.project_id");
  }

  const matchingClient = Array.isArray(config.client)
    ? config.client.find(client => client?.client_info?.android_client_info?.package_name === androidPackageName)
    : undefined;
  if (!matchingClient) {
    throw new Error(`Firebase app configuration must include Android package ${androidPackageName}`);
  }
  if (!isNonEmptyString(matchingClient.client_info?.mobilesdk_app_id)) {
    throw new Error(`Firebase app configuration for ${androidPackageName} must include client_info.mobilesdk_app_id`);
  }
  if (!Array.isArray(matchingClient.api_key) || !matchingClient.api_key.some(key => isNonEmptyString(key?.current_key))) {
    throw new Error(`Firebase app configuration for ${androidPackageName} must include a nonempty api_key`);
  }

  return config;
}

export async function writeFirebaseAppConfigFromEnvironment({ environment = process.env, outputPath = firebaseAppConfigPath } = {}) {
  const rawConfig = environment[firebaseAppConfigEnvironmentVariable];
  if (!isNonEmptyString(rawConfig)) {
    console.log("No Firebase Android app configuration supplied; building the self-hosting-capable artifact.");
    return false;
  }

  const config = validateFirebaseAppConfig(rawConfig);
  await writeFile(outputPath, `${JSON.stringify(config, null, 2)}\n`);
  return true;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await writeFirebaseAppConfigFromEnvironment();
}
