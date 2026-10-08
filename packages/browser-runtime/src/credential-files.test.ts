import { describe, expect, it, vi } from "vitest";
import { credentialFileBytes, importCredentialFile } from "./credential-files.js";

const credential = new TextEncoder().encode(JSON.stringify({
  version: 1,
  origin: "https://peppy.test",
  vaultId: "123e4567-e89b-12d3-a456-426614174000",
  deviceId: "123e4567-e89b-12d3-a456-426614174001",
  deviceToken: "a".repeat(96),
}));

describe("credential file import", () => {
  it("enforces the selected-file legacy bound from actual bytes", () => {
    expect(() => credentialFileBytes(new ArrayBuffer(1024 * 1024 + 1))).toThrow(expect.objectContaining({ code: "invalid-identity" }));
  });

  it("binds a portable credential to its origin before fetching its vault", async () => {
    const parse = vi.fn(async () => { throw { code: "credential-origin-mismatch" }; });
    const fetcher = vi.fn();
    const session = {
      parseCredentialFile: parse,
      portableIdentityMetadata: vi.fn(),
      enroll: vi.fn(),
    };

    await expect(importCredentialFile(session, credential, "https://peppy.test", "passphrase", fetcher)).rejects.toMatchObject({ code: "credential-origin-mismatch" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fetches a matching portable vault without redirects or cookies before enrollment", async () => {
    const metadata = { profile: {} };
    let request: RequestInit | undefined;
    const fetcher = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      request = init;
      return new Response(JSON.stringify({ vault_id: "123e4567-e89b-12d3-a456-426614174000", device_id: "123e4567-e89b-12d3-a456-426614174001", role: "owner", public_key_profile: {}, encrypted_vault_check_header: "e30=", profile_fingerprint: "fingerprint", key_epoch: 1 }), { status: 200 });
    }) as typeof fetch;
    const session = {
      parseCredentialFile: vi.fn(async () => ({ format: "portable" as const, deviceToken: "a".repeat(96) })),
      portableIdentityMetadata: vi.fn(async () => metadata),
      enroll: vi.fn(),
    };

    await importCredentialFile(session, credential, "https://peppy.test", "passphrase", fetcher);

    expect(request).toEqual(expect.objectContaining({ redirect: "error", credentials: "omit", cache: "no-store", referrerPolicy: "no-referrer" }));
    expect(request?.headers).toBeInstanceOf(Headers);
    expect(new Headers(request?.headers).get("Authorization")).toBe(`Bearer ${"a".repeat(96)}`);
    expect(session.portableIdentityMetadata).toHaveBeenCalledWith(credential, expect.any(Object));
    expect(session.enroll).toHaveBeenCalledWith(metadata, "a".repeat(96), "passphrase");
  });

  it("binds the built-in fetch receiver when no fetcher is injected", async () => {
    const original = globalThis.fetch;
    const receiver = vi.fn();
    globalThis.fetch = (function (this: typeof globalThis) {
      receiver(this);
      return Promise.resolve(new Response(JSON.stringify({ vault_id: "vault", device_id: "device", role: "owner", public_key_profile: {}, encrypted_vault_check_header: "e30=", profile_fingerprint: "fingerprint", key_epoch: 1 })));
    }) as typeof fetch;
    try {
      const session = {
        parseCredentialFile: vi.fn(async () => ({ format: "portable" as const, deviceToken: "a".repeat(96) })),
        portableIdentityMetadata: vi.fn(async () => ({})),
        enroll: vi.fn(),
      };
      await importCredentialFile(session, credential, "https://peppy.test", "passphrase");
      expect(receiver).toHaveBeenCalledWith(globalThis);
    } finally {
      globalThis.fetch = original;
    }
  });

  it("keeps legacy enrollment and Rust validator failures on their existing paths", async () => {
    const legacy = { metadata: { identity: "legacy" }, deviceToken: "legacy-token" };
    const session = {
      parseCredentialFile: vi.fn(async () => ({ format: "legacy" as const, ...legacy })),
      portableIdentityMetadata: vi.fn(),
      enroll: vi.fn(async () => { throw { code: "identity-exists" }; }),
    };
    await expect(importCredentialFile(session, credential, "https://peppy.test", "wrong")).rejects.toMatchObject({ code: "identity-exists" });
    expect(session.portableIdentityMetadata).not.toHaveBeenCalled();
  });
});
