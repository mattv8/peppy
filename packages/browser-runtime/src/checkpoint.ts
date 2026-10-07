import { BrowserCoreError, CoreRejectedError } from "./core.js";

const DATABASE_FILE = "client.db";
const CIPHER_PREFIX = "client.db.media/cipher/";
const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.ppss$/;
const SQLITE_HEADER = new TextEncoder().encode("SQLite format 3\0");
const MAX_WRAPPED_CREDENTIALS = 64 * 1024;
const MANIFEST_KEY = "manifest";

export interface CheckpointFile {
  path: string;
  bytes: Uint8Array;
}

export interface CheckpointInput {
  expectedGeneration: number;
  files: readonly CheckpointFile[];
  wrappedCredentials: Uint8Array;
  /** Existing immutable ciphertext files not hydrated into the worker's filesystem. */
  retainedCipherPaths?: readonly string[];
}

export interface CheckpointManifest {
  version: 1;
  generation: number;
  files: readonly string[];
  wrappedCredentials: Uint8Array;
}

export interface LoadedCheckpoint {
  manifest: CheckpointManifest;
  database: Uint8Array;
  readCipher(path: string): Promise<Uint8Array>;
}

export type CheckpointLoad = { kind: "absent" } | { kind: "present"; checkpoint: LoadedCheckpoint };

export class CheckpointError extends Error {
  public constructor(public readonly code: "storage-conflict" | "storage-unavailable" | "invalid-checkpoint") {
    super(code);
  }
}

interface StoredManifest {
  version: 1;
  generation: number;
  files: string[];
  wrappedCredentials: ArrayBuffer;
}

const copy = (bytes: Uint8Array): Uint8Array => new Uint8Array(bytes);
const copyBuffer = (bytes: Uint8Array): ArrayBuffer => Uint8Array.from(bytes).buffer;

function request<T>(value: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    value.onsuccess = () => resolve(value.result);
    value.onerror = () => reject(value.error);
  });
}

function transactionDone(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () => reject(transaction.error);
    transaction.onerror = () => reject(transaction.error);
  });
}

function validPath(path: string): boolean {
  return path === DATABASE_FILE || (
    path.startsWith(CIPHER_PREFIX) && CANONICAL_UUID.test(path.slice(CIPHER_PREFIX.length))
  );
}

function isPlainSqlite(bytes: Uint8Array): boolean {
  return SQLITE_HEADER.every((byte, index) => bytes[index] === byte);
}

function validGeneration(generation: unknown, minimum = 0): generation is number {
  return typeof generation === "number" && Number.isSafeInteger(generation) && generation >= minimum;
}

function validateInput(input: CheckpointInput): void {
  if (!validGeneration(input.expectedGeneration) || input.wrappedCredentials.byteLength === 0 ||
    input.wrappedCredentials.byteLength > MAX_WRAPPED_CREDENTIALS) {
    throw new CheckpointError("invalid-checkpoint");
  }
  const paths = new Set<string>();
  for (const file of input.files) {
    if (!validPath(file.path) || file.bytes.byteLength === 0 || paths.has(file.path)) {
      throw new CheckpointError("invalid-checkpoint");
    }
    paths.add(file.path);
  }
  for (const path of input.retainedCipherPaths ?? []) {
    if (!validPath(path) || path === DATABASE_FILE || paths.has(path)) {
      throw new CheckpointError("invalid-checkpoint");
    }
    paths.add(path);
  }
  const database = input.files.find((file) => file.path === DATABASE_FILE)?.bytes;
  if (!database || isPlainSqlite(database)) throw new CheckpointError("invalid-checkpoint");
}

function validateStoredManifest(value: unknown): StoredManifest {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new CheckpointError("invalid-checkpoint");
  }
  const manifest = value as Partial<StoredManifest>;
  if (manifest.version !== 1 || !validGeneration(manifest.generation, 1) ||
    !Array.isArray(manifest.files) || !(manifest.wrappedCredentials instanceof ArrayBuffer) ||
    manifest.wrappedCredentials.byteLength === 0 || manifest.wrappedCredentials.byteLength > MAX_WRAPPED_CREDENTIALS) {
    throw new CheckpointError("invalid-checkpoint");
  }
  const paths = new Set<string>();
  for (const path of manifest.files) {
    if (typeof path !== "string" || !validPath(path) || paths.has(path)) throw new CheckpointError("invalid-checkpoint");
    paths.add(path);
  }
  if (!paths.has(DATABASE_FILE)) throw new CheckpointError("invalid-checkpoint");
  return { version: 1, generation: manifest.generation, files: [...manifest.files], wrappedCredentials: manifest.wrappedCredentials.slice(0) };
}

async function abortAndSettle(transaction: IDBTransaction, completion: Promise<void>): Promise<void> {
  try {
    transaction.abort();
  } catch {
    // A transaction that already settled needs no further cancellation.
  }
  await completion.catch(() => undefined);
}

export class IndexedDbCheckpointStore {
  private connection?: IDBDatabase;
  private opening?: Promise<IDBDatabase>;

  public constructor(private readonly name: string, private readonly indexedDB: IDBFactory = globalThis.indexedDB) {}

  public async commit(input: CheckpointInput): Promise<number> {
    const expectedGeneration = input.expectedGeneration;
    validateInput(input);
    const files = input.files.map((file) => ({ path: file.path, bytes: copy(file.bytes) }));
    const retainedCipherPaths = [...(input.retainedCipherPaths ?? [])];
    const wrappedCredentials = copy(input.wrappedCredentials);
    try {
      const database = await this.open();
      const transaction = database.transaction("checkpoint", "readwrite", { durability: "strict" });
      const completion = transactionDone(transaction);
      try {
        const store = transaction.objectStore("checkpoint");
        const stored = await request(store.get(MANIFEST_KEY));
        const previous = stored === undefined ? undefined : validateStoredManifest(stored);
        const generation = previous?.generation ?? 0;
        if (generation !== expectedGeneration || generation === Number.MAX_SAFE_INTEGER) {
          await abortAndSettle(transaction, completion);
          throw new CheckpointError(generation !== expectedGeneration ? "storage-conflict" : "invalid-checkpoint");
        }
        const paths = new Set([...files.map((file) => file.path), ...retainedCipherPaths]);
        const previousPaths = new Set(previous?.files);
        if (retainedCipherPaths.some(path => !previousPaths.has(path))) {
          throw new CheckpointError("invalid-checkpoint");
        }
        const retainedKeys = await Promise.all(retainedCipherPaths.map(path => request(store.getKey(`file:${path}`))));
        if (retainedKeys.some(key => key === undefined)) {
          throw new CheckpointError("invalid-checkpoint");
        }
        for (const file of files) store.put(new Blob([copyBuffer(file.bytes)]), `file:${file.path}`);
        store.put({ version: 1, generation: generation + 1, files: [...paths], wrappedCredentials: copyBuffer(wrappedCredentials) } satisfies StoredManifest, MANIFEST_KEY);
        if (previous) for (const path of previous.files) if (!paths.has(path)) store.delete(`file:${path}`);
        await completion;
        return generation + 1;
      } catch (error: unknown) {
        await abortAndSettle(transaction, completion);
        if (error instanceof CheckpointError) throw error;
        throw new CheckpointError("storage-unavailable");
      }
    } finally {
      wrappedCredentials.fill(0);
      files.forEach((file) => file.bytes.fill(0));
    }
  }

  public async load(expectedGeneration?: number): Promise<CheckpointLoad> {
    const database = await this.open();
    let completion: Promise<void> | undefined;
    try {
      const transaction = database.transaction("checkpoint", "readonly");
      completion = transactionDone(transaction);
      const store = transaction.objectStore("checkpoint");
      const stored = await request(store.get(MANIFEST_KEY));
      if (stored === undefined) {
        await completion;
        return { kind: "absent" };
      }
      const manifest = validateStoredManifest(stored);
      if (expectedGeneration !== undefined && manifest.generation !== expectedGeneration) {
        throw new CheckpointError("storage-conflict");
      }
      const databaseBlob = await request(store.get(`file:${DATABASE_FILE}`)) as Blob | undefined;
      await completion;
      if (!(databaseBlob instanceof Blob)) throw new CheckpointError("invalid-checkpoint");
      const databaseBytes = new Uint8Array(await databaseBlob.arrayBuffer());
      if (databaseBytes.byteLength === 0 || isPlainSqlite(databaseBytes)) throw new CheckpointError("invalid-checkpoint");
      const publicManifest: CheckpointManifest = {
        version: 1,
        generation: manifest.generation,
        files: [...manifest.files],
        wrappedCredentials: new Uint8Array(manifest.wrappedCredentials.slice(0)),
      };
      return {
        kind: "present",
        checkpoint: {
          manifest: publicManifest,
          database: copy(databaseBytes),
          readCipher: (path) => this.readCipher(path, manifest.generation),
        },
      };
    } catch (error: unknown) {
      await completion?.catch(() => undefined);
      if (error instanceof CheckpointError) throw error;
      throw new CheckpointError("storage-unavailable");
    }
  }

  public close(): void {
    this.connection?.close();
    this.connection = undefined;
  }

  public async readCipher(path: string, expectedGeneration: number): Promise<Uint8Array> {
    if (!validPath(path) || path === DATABASE_FILE || !validGeneration(expectedGeneration, 1)) {
      throw new CheckpointError("invalid-checkpoint");
    }
    let completion: Promise<void> | undefined;
    try {
      const database = await this.open();
      const transaction = database.transaction("checkpoint", "readonly");
      completion = transactionDone(transaction);
      const store = transaction.objectStore("checkpoint");
      const stored = await request(store.get(MANIFEST_KEY));
      const current = validateStoredManifest(stored);
      if (current.generation !== expectedGeneration) throw new CheckpointError("storage-conflict");
      if (!current.files.includes(path)) throw new CheckpointError("invalid-checkpoint");
      const blob = await request(store.get(`file:${path}`)) as Blob | undefined;
      await completion;
      if (!(blob instanceof Blob)) throw new CheckpointError("invalid-checkpoint");
      return new Uint8Array(await blob.arrayBuffer());
    } catch (error: unknown) {
      await completion?.catch(() => undefined);
      if (error instanceof CheckpointError) throw error;
      throw new CheckpointError("storage-unavailable");
    }
  }

  private open(): Promise<IDBDatabase> {
    if (this.connection) return Promise.resolve(this.connection);
    this.opening ??= new Promise((resolve, reject) => {
      const open = this.indexedDB.open(this.name, 1);
      open.onupgradeneeded = () => open.result.createObjectStore("checkpoint");
      open.onsuccess = () => {
        const connection = open.result;
        connection.onversionchange = () => {
          if (this.connection === connection) this.close();
        };
        this.connection = connection;
        this.opening = undefined;
        resolve(connection);
      };
      open.onerror = () => {
        this.opening = undefined;
        reject(new CheckpointError("storage-unavailable"));
      };
    });
    return this.opening;
  }
}

export interface Mutation<T> {
  invoke(): Promise<T>;
  capture(expectedGeneration: number): Promise<CheckpointInput>;
}

export interface MutationResult<T> {
  value: T;
  generation: number;
}

export class RuntimeCoreError extends Error {
  public constructor() {
    super("Core operation failed");
  }
}

export class SerializedRuntime {
  private tail: Promise<void> = Promise.resolve();
  private poisoned = false;
  private terminated = false;

  public constructor(
    private readonly checkpoint: Pick<IndexedDbCheckpointStore, "commit">,
    private readonly terminate: () => void,
    private generation = 0,
  ) {}

  public read<T>(operation: () => Promise<T>): Promise<T> {
    return this.schedule(async () => {
      try {
        return await operation();
      } catch (error: unknown) {
        if (error instanceof CoreRejectedError) throw error;
        this.poison();
        if (error instanceof BrowserCoreError || error instanceof CheckpointError) throw error;
        throw new RuntimeCoreError();
      }
    });
  }

  public mutate<T>(mutation: Mutation<T>): Promise<MutationResult<T>> {
    return this.schedule(async () => {
      let value!: T;
      let coreFailure: Error | undefined;
      try {
        value = await mutation.invoke();
      } catch (error: unknown) {
        if (!(error instanceof CoreRejectedError)) {
          this.poison();
          throw error instanceof BrowserCoreError ? error : new RuntimeCoreError();
        }
        coreFailure = error;
      }
      try {
        const checkpoint = await mutation.capture(this.generation);
        const generation = await this.checkpoint.commit(checkpoint);
        this.generation = generation;
        if (coreFailure) throw coreFailure;
        return { value, generation };
      } catch (error: unknown) {
        if (error === coreFailure) throw error;
        this.poison();
        if (error instanceof CheckpointError) throw error;
        throw new CheckpointError("storage-unavailable");
      }
    });
  }

  /** Waits for accepted work before a host releases its exclusive owner lock. */
  public drain(): Promise<void> { return this.tail; }

  private schedule<T>(operation: () => Promise<T>): Promise<T> {
    const run = this.tail.then(async () => {
      if (this.poisoned) throw new CheckpointError("storage-unavailable");
      return operation();
    });
    this.tail = run.then(() => undefined, () => undefined);
    return run;
  }

  private poison(): void {
    this.poisoned = true;
    if (this.terminated) return;
    this.terminated = true;
    try {
      this.terminate();
    } catch {
      // Termination is best-effort and must not hide the storage failure.
    }
  }
}
