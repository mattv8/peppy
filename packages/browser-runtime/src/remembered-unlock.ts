const CACHE_DATABASE_SUFFIX = "-remembered-unlock-v1";
const CACHE_VERSION = 2;
const RECORD_STORE = "records";
const KEY_STORE = "keys";
const CURRENT_KEY = "current";
const SESSION_PATH = "/hosted/v1/web/app-session";
const ACCOUNT_PATH = "/hosted/v1/web/app-account";
const SESSION_TIMEOUT_MS = 5_000;
const OPERATION_TIMEOUT_MS = 10_000;
const MAX_WRAPPED_IDENTITY_BYTES = 64 * 1024;
const MAX_PASSPHRASE_BYTES = 4_096;
const AES_GCM_TAG_BYTES = 16;
const MAX_CIPHERTEXT_BYTES = MAX_PASSPHRASE_BYTES + AES_GCM_TAG_BYTES;
const ACCOUNT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

interface RememberedUnlockOptions {
  origin: string;
  fetch: typeof fetch;
  crypto?: Crypto;
}

interface StoredRecovery {
  version: 2;
  accountId: string;
  vaultId: string;
  origin: string;
  identityDigest: ArrayBuffer;
  iv: ArrayBuffer;
  ciphertext: ArrayBuffer;
}

type SessionResult = { kind: "authenticated"; accountId: string } | { kind: "clear" } | { kind: "transient" };
type AccountResult = { kind: "authenticated"; accountId: string; vaultId: string } | { kind: "clear" } | { kind: "transient" };

interface WrappedIdentity {
  vaultId: string;
}

function request<T>(value: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    value.onsuccess = () => resolve(value.result);
    value.onerror = () => reject(value.error);
  });
}

function transactionDone(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = transaction.onerror = () => reject(transaction.error);
  });
}

function copyBuffer(bytes: ArrayBuffer | Uint8Array): ArrayBuffer {
  return Uint8Array.from(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)).buffer;
}

function validBytes(value: unknown, length?: number): value is ArrayBuffer {
  return value instanceof ArrayBuffer && value.byteLength > 0 && (length === undefined || value.byteLength === length);
}

function validRecovery(value: unknown): value is StoredRecovery {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const recovery = value as Partial<StoredRecovery>;
  return recovery.version === CACHE_VERSION && typeof recovery.accountId === "string" && ACCOUNT_ID.test(recovery.accountId)
    && typeof recovery.vaultId === "string" && ACCOUNT_ID.test(recovery.vaultId)
    && typeof recovery.origin === "string" && validBytes(recovery.identityDigest, 32)
    && validBytes(recovery.iv, 12) && validBytes(recovery.ciphertext)
    && recovery.ciphertext.byteLength >= AES_GCM_TAG_BYTES && recovery.ciphertext.byteLength <= MAX_CIPHERTEXT_BYTES;
}

function parseWrappedIdentity(value: string): WrappedIdentity | undefined {
  if (new TextEncoder().encode(value).byteLength > MAX_WRAPPED_IDENTITY_BYTES) return undefined;
  try {
    const parsed: unknown = JSON.parse(value);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return undefined;
    const metadata = (parsed as { metadata?: unknown }).metadata;
    if (!metadata || typeof metadata !== "object" || Array.isArray(metadata)) return undefined;
    const vaultId = (metadata as { vaultId?: unknown }).vaultId;
    return typeof vaultId === "string" && ACCOUNT_ID.test(vaultId) ? { vaultId } : undefined;
  } catch {
    return undefined;
  }
}

function sameBytes(left: ArrayBuffer, right: ArrayBuffer): boolean {
  if (left.byteLength !== right.byteLength) return false;
  const leftBytes = new Uint8Array(left);
  const rightBytes = new Uint8Array(right);
  let difference = 0;
  for (let index = 0; index < leftBytes.length; index += 1) difference |= leftBytes[index]! ^ rightBytes[index]!;
  return difference === 0;
}

function associatedData(accountId: string, origin: string, identityDigest: ArrayBuffer): Uint8Array {
  const digest = [...new Uint8Array(identityDigest)].map(byte => byte.toString(16).padStart(2, "0")).join("");
  return new TextEncoder().encode(`peppy.remembered-unlock.v${CACHE_VERSION}\0${accountId}\0${origin}\0${digest}`);
}

export class RememberedUnlock {
  private readonly origin: string;
  private readonly databaseName: string;
  private readonly crypto: Crypto | undefined;
  private database?: IDBDatabase;
  private opening?: Promise<IDBDatabase>;
  private storageTail: Promise<void> = Promise.resolve();
  private fence = 0;

  public constructor(databaseName: string, private readonly options: RememberedUnlockOptions) {
    this.origin = new URL(options.origin).origin;
    this.databaseName = `${databaseName}${CACHE_DATABASE_SUFFIX}`;
    this.crypto = options.crypto ?? globalThis.crypto;
  }

  public async remember(wrappedIdentity: string, passphrase: string): Promise<void> {
    await this.withDeadline(() => this.rememberInternal(wrappedIdentity, passphrase)).catch(() => undefined);
  }

  private async rememberInternal(wrappedIdentity: string, passphrase: string): Promise<void> {
    const fence = this.fence;
    const identity = parseWrappedIdentity(wrappedIdentity);
    const phrase = new TextEncoder().encode(passphrase);
    if (!identity || phrase.byteLength > MAX_PASSPHRASE_BYTES) {
      phrase.fill(0);
      return;
    }
    const session = await this.authenticatedOwner(identity.vaultId);
    if (session.kind === "clear") {
      phrase.fill(0);
      return this.clear();
    }
    if (session.kind !== "authenticated" || !this.crypto) {
      phrase.fill(0);
      return;
    }

    try {
      const identityDigest = await this.crypto.subtle.digest("SHA-256", new TextEncoder().encode(wrappedIdentity));
      const key = await this.crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
      const iv = this.crypto.getRandomValues(new Uint8Array(12));
      const aad = associatedData(session.accountId, this.origin, identityDigest);
      const ciphertext = await this.crypto.subtle.encrypt(
        { name: "AES-GCM", iv: copyBuffer(iv), additionalData: copyBuffer(aad) },
        key,
        copyBuffer(phrase),
      );
      await this.enqueue(async () => {
        if (fence !== this.fence) return;
        const current = await this.authenticatedOwner(identity.vaultId);
        if (current.kind === "clear") {
          await this.deleteDirect();
          return;
        }
        if (current.kind !== "authenticated" || current.accountId !== session.accountId || fence !== this.fence) return;
        const recovery: StoredRecovery = {
          version: CACHE_VERSION,
          accountId: session.accountId,
          vaultId: identity.vaultId,
          origin: this.origin,
          identityDigest: copyBuffer(identityDigest),
          iv: copyBuffer(iv),
          ciphertext: copyBuffer(ciphertext),
        };
        const database = await this.open();
        const transaction = database.transaction([RECORD_STORE, KEY_STORE], "readwrite", { durability: "strict" });
        const completion = transactionDone(transaction);
        transaction.objectStore(KEY_STORE).put(key, CURRENT_KEY);
        transaction.objectStore(RECORD_STORE).put(recovery, CURRENT_KEY);
        await completion;
      });
    } catch {
      // Recovery is a convenience cache; storage and crypto failures leave normal unlock available.
    } finally {
      phrase.fill(0);
    }
  }

  public async recover(wrappedIdentity: string): Promise<string | undefined> {
    return this.withDeadline(() => this.recoverInternal(wrappedIdentity)).catch(() => undefined);
  }

  private async recoverInternal(wrappedIdentity: string): Promise<string | undefined> {
    const fence = this.fence;
    const identity = parseWrappedIdentity(wrappedIdentity);
    if (!identity) return undefined;
    const recovery = await this.readRecovery();
    if (fence !== this.fence || !recovery || !this.crypto) return undefined;
    const session = await this.authenticatedOwner(identity.vaultId);
    if (session.kind === "clear" || (session.kind === "authenticated" && session.accountId !== recovery.accountId)) {
      await this.clear();
      return undefined;
    }
    if (session.kind !== "authenticated" || fence !== this.fence) return undefined;
    try {
      const identityDigest = await this.crypto.subtle.digest("SHA-256", new TextEncoder().encode(wrappedIdentity));
      if (fence !== this.fence || recovery.origin !== this.origin || !sameBytes(recovery.identityDigest, identityDigest)) {
        await this.clear();
        return undefined;
      }
      const key = await this.readKey();
      if (fence !== this.fence || !(key instanceof CryptoKey) || key.type !== "secret" || key.extractable || !key.usages.includes("decrypt")) {
        await this.clear();
        return undefined;
      }
      const plaintext = await this.crypto.subtle.decrypt(
        {
          name: "AES-GCM",
          iv: recovery.iv,
          additionalData: copyBuffer(associatedData(recovery.accountId, recovery.origin, recovery.identityDigest)),
        },
        key,
        recovery.ciphertext,
      );
      const phrase = new Uint8Array(plaintext);
      try {
        const passphrase = new TextDecoder("utf-8", { fatal: true }).decode(phrase);
        const current = await this.authenticatedOwner(identity.vaultId);
        if (fence !== this.fence || current.kind !== "authenticated" || current.accountId !== recovery.accountId) {
          if (current.kind === "clear") await this.clear();
          return undefined;
        }
        return passphrase;
      } finally {
        phrase.fill(0);
      }
    } catch {
      await this.clear();
      return undefined;
    }
  }

  public async clear(): Promise<void> {
    this.fence += 1;
    await this.withDeadline(() => this.enqueue(() => this.deleteDirect()));
  }

  public async validate(): Promise<void> {
    await this.withDeadline(() => this.validateInternal()).catch(() => undefined);
  }

  private async validateInternal(): Promise<void> {
    const recovery = await this.readRecovery();
    if (!recovery) return;
    const session = await this.authenticatedOwner(recovery.vaultId);
    if (session.kind === "clear" || (session.kind === "authenticated" && session.accountId !== recovery.accountId)) await this.clear();
  }

  private async session(): Promise<SessionResult> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), SESSION_TIMEOUT_MS);
    try {
      const response = await this.options.fetch(new URL(SESSION_PATH, this.origin).toString(), {
        credentials: "include",
        cache: "no-store",
        redirect: "error",
        signal: controller.signal,
      });
      if (response.status === 401) return { kind: "clear" };
      if (!response.ok) return { kind: "transient" };
      const value: unknown = await response.json();
      if (!value || typeof value !== "object" || Array.isArray(value)) return { kind: "transient" };
      const session = value as { signed_in?: unknown; account_id?: unknown };
      if (session.signed_in === false) return { kind: "clear" };
      if (session.signed_in !== true || typeof session.account_id !== "string" || !ACCOUNT_ID.test(session.account_id)) return { kind: "transient" };
      return { kind: "authenticated", accountId: session.account_id };
    } catch {
      return { kind: "transient" };
    } finally {
      clearTimeout(timer);
    }
  }

  private async authenticatedOwner(vaultId: string): Promise<SessionResult> {
    const session = await this.session();
    if (session.kind !== "authenticated") return session;
    const account = await this.account();
    if (account.kind !== "authenticated") return account;
    return account.accountId === session.accountId && account.vaultId === vaultId ? session : { kind: "clear" };
  }

  private async account(): Promise<AccountResult> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), SESSION_TIMEOUT_MS);
    try {
      const response = await this.options.fetch(new URL(ACCOUNT_PATH, this.origin).toString(), {
        credentials: "include",
        cache: "no-store",
        redirect: "error",
        signal: controller.signal,
      });
      if (response.status === 401) return { kind: "clear" };
      if (!response.ok) return { kind: "transient" };
      const value: unknown = await response.json();
      if (!value || typeof value !== "object" || Array.isArray(value)) return { kind: "transient" };
      const account = value as { account_id?: unknown; vault_id?: unknown };
      if (typeof account.account_id !== "string" || !ACCOUNT_ID.test(account.account_id)
        || typeof account.vault_id !== "string" || !ACCOUNT_ID.test(account.vault_id)) return { kind: "transient" };
      return { kind: "authenticated", accountId: account.account_id, vaultId: account.vault_id };
    } catch {
      return { kind: "transient" };
    } finally {
      clearTimeout(timer);
    }
  }

  private async readRecovery(): Promise<StoredRecovery | undefined> {
    try {
      const database = await this.open();
      const transaction = database.transaction(RECORD_STORE, "readonly");
      const completion = transactionDone(transaction);
      const stored = await request(transaction.objectStore(RECORD_STORE).get(CURRENT_KEY));
      await completion;
      if (stored === undefined) return undefined;
      if (validRecovery(stored)) return stored;
    } catch {
      return undefined;
    }
    await this.clear();
    return undefined;
  }

  private async readKey(): Promise<unknown> {
    const database = await this.open();
    const transaction = database.transaction(KEY_STORE, "readonly");
    const completion = transactionDone(transaction);
    const key = await request(transaction.objectStore(KEY_STORE).get(CURRENT_KEY));
    await completion;
    return key;
  }

  private async deleteDirect(): Promise<void> {
    const database = await this.open();
    const transaction = database.transaction([RECORD_STORE, KEY_STORE], "readwrite", { durability: "strict" });
    const completion = transactionDone(transaction);
    transaction.objectStore(RECORD_STORE).delete(CURRENT_KEY);
    transaction.objectStore(KEY_STORE).delete(CURRENT_KEY);
    await completion;
  }

  private enqueue(operation: () => Promise<void>): Promise<void> {
    const run = this.storageTail.then(operation, operation);
    this.storageTail = run.catch(() => undefined);
    return run;
  }

  private async withDeadline<T>(operation: () => Promise<T>): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.fence += 1;
        reject(new Error("remembered-unlock-timeout"));
      }, OPERATION_TIMEOUT_MS);
      void operation().then(
        value => { clearTimeout(timer); resolve(value); },
        error => { clearTimeout(timer); reject(error); },
      );
    });
  }

  private open(): Promise<IDBDatabase> {
    if (this.database) return Promise.resolve(this.database);
    this.opening ??= new Promise((resolve, reject) => {
      const request = indexedDB.open(this.databaseName, CACHE_VERSION);
      request.onupgradeneeded = () => {
        if (!request.result.objectStoreNames.contains(RECORD_STORE)) request.result.createObjectStore(RECORD_STORE);
        if (!request.result.objectStoreNames.contains(KEY_STORE)) request.result.createObjectStore(KEY_STORE);
      };
      request.onsuccess = () => {
        this.database = request.result;
        this.database.onversionchange = () => {
          this.database?.close();
          this.database = undefined;
        };
        this.opening = undefined;
        resolve(request.result);
      };
      request.onerror = () => {
        this.opening = undefined;
        reject(request.error);
      };
    });
    return this.opening;
  }
}
