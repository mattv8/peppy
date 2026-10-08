import { OriginTransport, TransportError } from "./transport.js";

const MAX_CREDENTIAL_FILE_BYTES = 1024 * 1024;

export type ParsedCredentialFile =
  | { format: "legacy"; metadata: unknown; deviceToken: string }
  | { format: "portable"; deviceToken: string };

export interface CredentialFileSession {
  parseCredentialFile(bytes: Uint8Array): Promise<ParsedCredentialFile>;
  portableIdentityMetadata(bytes: Uint8Array, vault: Record<string, unknown>): Promise<unknown>;
  enroll(metadata: unknown, deviceToken: string, passphrase: string): Promise<void>;
}

export function credentialFileBytes(value: unknown): Uint8Array {
  if (!(value instanceof ArrayBuffer) || value.byteLength === 0 || value.byteLength > MAX_CREDENTIAL_FILE_BYTES) {
    throw { code: "invalid-identity" };
  }
  return new Uint8Array(value);
}

/** Imports a Worker-owned credential: Rust validates the file before a candidate token is used. */
export async function importCredentialFile(
  session: CredentialFileSession,
  bytes: Uint8Array,
  origin: string,
  passphrase: string,
  fetcher?: typeof fetch,
): Promise<void> {
  const parsed = await session.parseCredentialFile(bytes);
  if (parsed.format === "legacy") {
    await session.enroll(parsed.metadata, parsed.deviceToken, passphrase);
    return;
  }

  const transport = new OriginTransport({ origin, deviceToken: () => parsed.deviceToken, ...(fetcher ? { fetch: fetcher } : {}) });
  let vault: Record<string, unknown>;
  try {
    vault = await transport.json<Record<string, unknown>>("/v1/vault", { method: "GET" });
  } catch (error: unknown) {
    if (error instanceof TransportError) throw { code: "credential-network" };
    throw error;
  }
  const metadata = await session.portableIdentityMetadata(bytes, vault);
  await session.enroll(metadata, parsed.deviceToken, passphrase);
}
