import { type CheckpointFile, CheckpointError, type CheckpointInput, type LoadedCheckpoint } from "./checkpoint.js";
import { type CoreResult } from "./core.js";

const ROOT = "/peppy";
const DATABASE = "client.db";
const CIPHER_DIRECTORY = "client.db.media/cipher";
const PAGE_LIMIT = 1_000;
const MAX_METADATA_PAGES = 100;
const MAX_WRAPPED_IDENTITY_BYTES = 64 * 1024;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const SQLITE_HEADER = new TextEncoder().encode("SQLite format 3\0");

export interface EmscriptenFsStat {
  size: number;
  mode: number;
}

/** The intentionally narrow portion of Emscripten's MEMFS exposed to checkpoint code. */
export interface EmscriptenFilesystem {
  readdir(path: string): string[];
  lstat(path: string): EmscriptenFsStat;
  isFile(mode: number): boolean;
  isDir(mode: number): boolean;
  isLink(mode: number): boolean;
  readFile(path: string): Uint8Array;
  writeFile(path: string, bytes: Uint8Array): void;
  mkdir(path: string): void;
  unlink(path: string): void;
}

export interface CheckpointCore {
  invoke(request: { command: string; args: Record<string, unknown> }): Promise<CoreResult>;
}

export interface CaptureFilesystemCheckpointOptions {
  fs: EmscriptenFilesystem;
  core: CheckpointCore;
  expectedGeneration: number;
  previousFiles: readonly string[];
}

export interface CipherCheckpointStore {
  readCipher(path: string, generation: number): Promise<Uint8Array>;
}

function invalid(): CheckpointError {
  return new CheckpointError("invalid-checkpoint");
}

function child(parent: string, name: string): string {
  return `${parent}/${name}`;
}

function cipherPath(id: string): string {
  return `${CIPHER_DIRECTORY}/${id}.ppss`;
}

function validCipherPath(path: string): boolean {
  return path.startsWith(`${CIPHER_DIRECTORY}/`) && UUID.test(path.slice(CIPHER_DIRECTORY.length + 1, -5)) && path.endsWith(".ppss");
}

function copied(bytes: Uint8Array): Uint8Array {
  return new Uint8Array(bytes);
}

function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  return left.byteLength === right.byteLength && left.every((byte, index) => byte === right[index]);
}

function containsPlainSqlite(bytes: Uint8Array): boolean {
  return SQLITE_HEADER.every((byte, index) => bytes[index] === byte);
}

function directoryEntries(fs: EmscriptenFilesystem, path: string): Set<string> {
  const entries = fs.readdir(path);
  if (!Array.isArray(entries) || entries.some(entry => typeof entry !== "string" || entry.includes("/"))) throw invalid();
  return new Set(entries.filter(entry => entry !== "." && entry !== ".."));
}

function assertDirectory(fs: EmscriptenFilesystem, path: string): void {
  const stat = fs.lstat(path);
  if (fs.isLink(stat.mode) || !fs.isDir(stat.mode)) throw invalid();
}

function assertFile(fs: EmscriptenFilesystem, path: string): EmscriptenFsStat {
  const stat = fs.lstat(path);
  if (fs.isLink(stat.mode) || !fs.isFile(stat.mode) || !Number.isSafeInteger(stat.size) || stat.size < 0) throw invalid();
  return stat;
}

function readStableFile(fs: EmscriptenFilesystem, path: string): Uint8Array {
  const before = assertFile(fs, path);
  const bytes = copied(fs.readFile(path));
  const after = assertFile(fs, path);
  if (before.size !== after.size || after.size !== bytes.byteLength) throw invalid();
  return bytes;
}

function mediaDirectory(fs: EmscriptenFilesystem, rootEntries: Set<string>): Set<string> | undefined {
  const mediaName = "client.db.media";
  if (!rootEntries.has(mediaName)) return undefined;
  const media = child(ROOT, mediaName);
  assertDirectory(fs, media);
  const entries = directoryEntries(fs, media);
  if (!entries.has("cipher")) return undefined;
  const cipher = child(media, "cipher");
  assertDirectory(fs, cipher);
  return directoryEntries(fs, cipher);
}

function checkpointIdentity(result: CoreResult): Uint8Array {
  if (typeof result !== "object" || result === null || Array.isArray(result)) throw invalid();
  const wrappedIdentity = (result as { wrappedIdentity?: unknown }).wrappedIdentity;
  if (typeof wrappedIdentity !== "string") throw invalid();
  const bytes = new TextEncoder().encode(wrappedIdentity);
  if (bytes.byteLength === 0 || bytes.byteLength > MAX_WRAPPED_IDENTITY_BYTES) throw invalid();
  return bytes;
}

function attachmentIds(result: CoreResult, after: string | undefined): string[] {
  if (typeof result !== "object" || result === null || Array.isArray(result)) throw invalid();
  const ids = (result as { attachmentIds?: unknown }).attachmentIds;
  if (!Array.isArray(ids) || ids.length > PAGE_LIMIT || ids.some(id => typeof id !== "string" || !UUID.test(id))) throw invalid();
  let previous = after;
  for (const id of ids) {
    if (previous !== undefined && id <= previous) throw invalid();
    previous = id;
  }
  return ids as string[];
}

async function localAttachmentIds(core: CheckpointCore): Promise<string[]> {
  const ids: string[] = [];
  let after: string | undefined;
  for (let page = 0; page < MAX_METADATA_PAGES; page++) {
    const args: Record<string, unknown> = { limit: PAGE_LIMIT };
    if (after !== undefined) args.after = after;
    const batch = attachmentIds(await core.invoke({ command: "_worker_local_attachment_ids", args }), after);
    ids.push(...batch);
    if (batch.length < PAGE_LIMIT) return ids;
    after = batch.at(-1);
  }
  throw invalid();
}

function createDirectory(fs: EmscriptenFilesystem, parent: string, name: string): void {
  const entries = directoryEntries(fs, parent);
  const path = child(parent, name);
  if (!entries.has(name)) fs.mkdir(path);
  assertDirectory(fs, path);
}

function ensureCipherDirectory(fs: EmscriptenFilesystem): void {
  const rootEntries = directoryEntries(fs, "/");
  if (!rootEntries.has("peppy")) fs.mkdir(ROOT);
  assertDirectory(fs, ROOT);
  createDirectory(fs, ROOT, "client.db.media");
  createDirectory(fs, child(ROOT, "client.db.media"), "cipher");
}

/** Captures only the encrypted database, canonical ciphertext, and opaque Rust identity envelope. */
export async function captureFilesystemCheckpoint(options: CaptureFilesystemCheckpointOptions): Promise<CheckpointInput> {
  const previousFiles = new Set(options.previousFiles);
  if ([...previousFiles].some(path => path !== DATABASE && !validCipherPath(path))) throw invalid();
  assertDirectory(options.fs, ROOT);
  const rootEntries = directoryEntries(options.fs, ROOT);
  if (!rootEntries.has(DATABASE) || rootEntries.has(`${DATABASE}-journal`) || rootEntries.has(`${DATABASE}-wal`) || rootEntries.has(`${DATABASE}-shm`)) throw invalid();
  const database = readStableFile(options.fs, child(ROOT, DATABASE));
  if (database.byteLength === 0 || containsPlainSqlite(database)) throw invalid();

  const [identity, ids] = await Promise.all([
    options.core.invoke({ command: "_worker_checkpoint_identity", args: {} }).then(checkpointIdentity),
    localAttachmentIds(options.core),
  ]);
  const cipherEntries = mediaDirectory(options.fs, rootEntries);
  const files: CheckpointFile[] = [{ path: DATABASE, bytes: database }];
  const retainedCipherPaths: string[] = [];
  for (const id of ids) {
    const path = cipherPath(id);
    const name = `${id}.ppss`;
    if (cipherEntries?.has(name)) {
      files.push({ path, bytes: readStableFile(options.fs, child(ROOT, path)) });
    } else if (previousFiles.has(path)) {
      retainedCipherPaths.push(path);
    } else {
      throw invalid();
    }
  }
  return { expectedGeneration: options.expectedGeneration, files, wrappedCredentials: identity, retainedCipherPaths };
}

/** Restores the SQLCipher database into a fresh module; ciphertext remains lazy in checkpoint storage. */
export function restoreFilesystemCheckpoint(checkpoint: LoadedCheckpoint, fs: EmscriptenFilesystem): void {
  if (!Number.isSafeInteger(checkpoint.manifest.generation) || checkpoint.manifest.generation < 1 ||
    checkpoint.database.byteLength === 0 || containsPlainSqlite(checkpoint.database)) throw invalid();
  ensureCipherDirectory(fs);
  const rootEntries = directoryEntries(fs, ROOT);
  if (rootEntries.has(DATABASE)) throw invalid();
  fs.writeFile(child(ROOT, DATABASE), copied(checkpoint.database));
}

/** Lazily materializes a single immutable ciphertext object without decrypting it. */
export async function hydrateCipher(id: string, expectedGeneration: number, store: CipherCheckpointStore, fs: EmscriptenFilesystem): Promise<void> {
  if (!UUID.test(id) || !Number.isSafeInteger(expectedGeneration) || expectedGeneration < 1) throw invalid();
  const path = cipherPath(id);
  const bytes = copied(await store.readCipher(path, expectedGeneration));
  if (bytes.byteLength === 0) throw invalid();
  ensureCipherDirectory(fs);
  const directory = child(ROOT, CIPHER_DIRECTORY);
  const entries = directoryEntries(fs, directory);
  const name = `${id}.ppss`;
  const destination = child(ROOT, path);
  if (entries.has(name)) {
    if (!sameBytes(readStableFile(fs, destination), bytes)) throw invalid();
    return;
  }
  fs.writeFile(destination, bytes);
}
