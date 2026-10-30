import "fake-indexeddb/auto";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RememberedUnlock } from "./remembered-unlock.js";

const origin = "https://peppy.test";
const account = "11111111-1111-7111-8111-111111111111";
const otherAccount = "22222222-2222-7222-8222-222222222222";
const vault = "33333333-3333-7333-8333-333333333333";
const identity = JSON.stringify({ metadata: { vaultId: vault, deviceId: "device", epoch: "1" } });
const rotatedIdentity = JSON.stringify({ metadata: { vaultId: vault, deviceId: "device", epoch: "2" } });

function sessionFetch(
  session: unknown = { signed_in: true, account_id: account },
  sessionStatus = 200,
  appAccount: unknown = { account_id: account, vault_id: vault },
  appAccountStatus = 200,
): typeof fetch {
  return vi.fn(async input => {
    const path = new URL(String(input)).pathname;
    const [body, status] = path === "/hosted/v1/web/app-session" ? [session, sessionStatus] : [appAccount, appAccountStatus];
    return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  }) as unknown as typeof fetch;
}

function unlock(fetch = sessionFetch(), databaseName = `remembered-${crypto.randomUUID()}`): RememberedUnlock {
  return new RememberedUnlock(databaseName, { origin, fetch, crypto });
}

async function cacheRecords(databaseName: string): Promise<{ record: unknown; key: CryptoKey | undefined }> {
  const database = await new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open(`${databaseName}-remembered-unlock-v1`, 1);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  if (!database.objectStoreNames.contains("records") || !database.objectStoreNames.contains("keys")) {
    database.close();
    return { record: undefined, key: undefined };
  }
  const transaction = database.transaction(["records", "keys"], "readonly");
  const recordRequest = transaction.objectStore("records").get("current");
  const keyRequest = transaction.objectStore("keys").get("current");
  const result = await new Promise<{ record: unknown; key: CryptoKey | undefined }>((resolve, reject) => {
    transaction.oncomplete = () => resolve({ record: recordRequest.result, key: keyRequest.result });
    transaction.onabort = transaction.onerror = () => reject(transaction.error);
  });
  database.close();
  return result;
}

describe("RememberedUnlock", () => {
  afterEach(() => vi.restoreAllMocks());

  it("restores a phrase only for the authenticated account", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");

    await expect(unlock(sessionFetch(), databaseName).recover(identity)).resolves.toBe("phrase");
  });

  it("deletes recovery when the session is signed out or belongs to another account", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");

    await expect(unlock(sessionFetch({ signed_in: false }), databaseName).recover(identity)).resolves.toBeUndefined();
    expect((await cacheRecords(databaseName)).record).toBeUndefined();

    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");
    await expect(unlock(sessionFetch({ signed_in: true, account_id: otherAccount }), databaseName).recover(identity)).resolves.toBeUndefined();
    expect((await cacheRecords(databaseName)).record).toBeUndefined();
  });

  it("deletes recovery after a definitive 401", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");

    await expect(unlock(sessionFetch({ error: "unauthenticated" }, 401), databaseName).recover(identity)).resolves.toBeUndefined();
    expect((await cacheRecords(databaseName)).record).toBeUndefined();
  });

  it("preserves recovery on a transient session outage", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");

    await expect(unlock(sessionFetch({ error: "unavailable" }, 503), databaseName).recover(identity)).resolves.toBeUndefined();
    expect((await cacheRecords(databaseName)).record).toBeDefined();
  });

  it("does not cache a phrase when the authenticated account does not own the identity vault", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(undefined, 200, { account_id: account, vault_id: otherAccount }), databaseName).remember(identity, "phrase");

    expect((await cacheRecords(databaseName)).record).toBeUndefined();
  });

  it("does not allocate a cache record for oversized secrets or identities", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    const cache = unlock(sessionFetch(), databaseName);
    await cache.remember(identity, "p".repeat(4097));
    await cache.remember(`${identity}${"x".repeat(64 * 1024)}`, "phrase");

    expect((await cacheRecords(databaseName)).record).toBeUndefined();
  });

  it("fails closed and deletes an authenticated-tampered ciphertext", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");
    const database = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(`${databaseName}-remembered-unlock-v1`, 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    const transaction = database.transaction("records", "readwrite");
    const store = transaction.objectStore("records");
    const request = store.get("current");
    await new Promise<void>((resolve, reject) => {
      request.onsuccess = () => {
        const record = request.result as { ciphertext: ArrayBuffer };
        new Uint8Array(record.ciphertext)[0] ^= 1;
        store.put(record, "current");
      };
      transaction.oncomplete = () => resolve();
      transaction.onabort = transaction.onerror = () => reject(transaction.error);
    });
    database.close();

    await expect(unlock(sessionFetch(), databaseName).recover(identity)).resolves.toBeUndefined();
    expect((await cacheRecords(databaseName)).record).toBeUndefined();
  });

  it("binds recovery to the canonical origin and exact wrapped identity", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");

    await expect(unlock(sessionFetch(), databaseName).recover(rotatedIdentity)).resolves.toBeUndefined();
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");
    const otherOrigin = new RememberedUnlock(databaseName, { origin: "https://other.peppy.test", fetch: sessionFetch(), crypto });
    await expect(otherOrigin.recover(identity)).resolves.toBeUndefined();
  });

  it("stores only ciphertext and a nonextractable encryption key", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "a private phrase");

    const { record, key } = await cacheRecords(databaseName);
    expect(JSON.stringify(record)).not.toContain("a private phrase");
    expect(key?.extractable).toBe(false);
    await expect(crypto.subtle.exportKey("raw", key!)).rejects.toThrow();
  });

  it("fences a pending save when cleared", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    let release!: (digest: ArrayBuffer) => void;
    const gate = new Promise<ArrayBuffer>(resolve => { release = resolve; });
    const subtle = crypto.subtle;
    vi.spyOn(subtle, "digest").mockImplementationOnce(() => gate);
    const cache = unlock(sessionFetch(), databaseName);
    const save = cache.remember(identity, "phrase");
    await Promise.resolve();
    await cache.clear();
    release(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(identity)));
    await save;

    expect((await cacheRecords(databaseName)).record).toBeUndefined();
  });

  it("does not return a decrypted phrase when clear fences recovery", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");
    let release!: () => void;
    const recheck = new Promise<void>(resolve => { release = resolve; });
    let requests = 0;
    const fetcher = vi.fn(async input => {
      requests += 1;
      if (requests === 3) await recheck;
      const path = new URL(String(input)).pathname;
      const body = path === "/hosted/v1/web/app-session" ? { signed_in: true, account_id: account } : { account_id: account, vault_id: vault };
      return new Response(JSON.stringify(body));
    }) as unknown as typeof fetch;
    const cache = unlock(fetcher, databaseName);
    const recovery = cache.recover(identity);
    await vi.waitFor(() => expect(requests).toBe(3));
    const cleared = cache.clear();
    release();
    await cleared;

    await expect(recovery).resolves.toBeUndefined();
  });

  it("uses no-store credentialed requests that reject redirects", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    const fetcher = sessionFetch();
    await unlock(fetcher, databaseName).remember(identity, "phrase");

    expect(fetcher).toHaveBeenCalledWith(`${origin}/hosted/v1/web/app-session`, expect.objectContaining({ credentials: "include", cache: "no-store", redirect: "error" }));
    expect(fetcher).toHaveBeenCalledWith(`${origin}/hosted/v1/web/app-account`, expect.objectContaining({ credentials: "include", cache: "no-store", redirect: "error" }));
  });

  it("aborts a bounded session request and preserves the cache", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), databaseName).remember(identity, "phrase");
    vi.spyOn(globalThis, "setTimeout").mockImplementation(((callback: TimerHandler) => {
      queueMicrotask(() => typeof callback === "function" && callback());
      return 0 as unknown as ReturnType<typeof setTimeout>;
    }) as unknown as typeof setTimeout);
    const fetcher = vi.fn((_input: RequestInfo | URL, init?: RequestInit) => new Promise<Response>((_resolve, reject) => {
      init?.signal?.addEventListener("abort", () => reject(new DOMException("timed out", "AbortError")));
    })) as unknown as typeof fetch;
    const recovery = unlock(fetcher, databaseName).recover(identity);
    await expect(recovery).resolves.toBeUndefined();
    expect((await cacheRecords(databaseName)).record).toBeDefined();
  });

  it("times out optional recovery and fences a late save", async () => {
    const databaseName = `remembered-${crypto.randomUUID()}`;
    const subtle = crypto.subtle;
    const digest = subtle.digest.bind(subtle);
    let release!: (value: ArrayBuffer) => void;
    let expire!: () => void;
    vi.spyOn(globalThis, "setTimeout").mockImplementation(((callback: TimerHandler, delay?: number) => {
      if (delay === 10_000) expire = callback as () => void;
      return 0 as unknown as ReturnType<typeof setTimeout>;
    }) as unknown as typeof setTimeout);
    const delayedDigest = new Promise<ArrayBuffer>(resolve => { release = resolve; });
    let digestStarted = false;
    vi.spyOn(subtle, "digest").mockImplementationOnce(() => { digestStarted = true; return delayedDigest; });
    const cache = unlock(sessionFetch(), databaseName);

    const save = cache.remember(identity, "phrase");
    await vi.waitFor(() => expect(digestStarted).toBe(true));
    expire();
    await save;
    release(await digest("SHA-256", new TextEncoder().encode(identity)));
    await vi.waitFor(async () => expect((await cacheRecords(databaseName)).record).toBeUndefined());

    expect((await cacheRecords(databaseName)).record).toBeUndefined();
    vi.restoreAllMocks();
    const recoveryDatabaseName = `remembered-${crypto.randomUUID()}`;
    await unlock(sessionFetch(), recoveryDatabaseName).remember(identity, "phrase");
    vi.spyOn(globalThis, "setTimeout").mockImplementation(((callback: TimerHandler, delay?: number) => {
      if (delay === 10_000) expire = callback as () => void;
      return 0 as unknown as ReturnType<typeof setTimeout>;
    }) as unknown as typeof setTimeout);
    let releaseDecrypt!: (value: ArrayBuffer) => void;
    const delayedDecrypt = new Promise<ArrayBuffer>(resolve => { releaseDecrypt = resolve; });
    let decryptStarted = false;
    vi.spyOn(subtle, "decrypt").mockImplementationOnce(() => { decryptStarted = true; return delayedDecrypt; });
    const recovery = unlock(sessionFetch(), recoveryDatabaseName).recover(identity);
    await vi.waitFor(() => expect(decryptStarted).toBe(true));
    expire();

    await expect(recovery).resolves.toBeUndefined();
    releaseDecrypt(new ArrayBuffer(0));
  });
});
