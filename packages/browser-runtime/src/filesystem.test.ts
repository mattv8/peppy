import { describe, expect, it } from "vitest";
import { CheckpointError, type LoadedCheckpoint } from "./checkpoint.js";
import { captureFilesystemCheckpoint, hydrateCipher, restoreFilesystemCheckpoint, type CheckpointCore } from "./filesystem.js";

const encoder = new TextEncoder();
const id = "123e4567-e89b-12d3-a456-426614174000";
const coldId = "00000000-0000-4000-8000-000000000000";
const cipher = (attachmentId: string): string => `client.db.media/cipher/${attachmentId}.ppss`;

import { MemoryFs } from "./test-support/memory-fs.js";

function seededFs(): MemoryFs {
  const fs = new MemoryFs();
  fs.mkdir("/peppy");
  fs.writeFile("/peppy/client.db", encoder.encode("encrypted database"));
  fs.mkdir("/peppy/client.db.media");
  fs.mkdir("/peppy/client.db.media/cipher");
  fs.writeFile(`/peppy/${cipher(id)}`, encoder.encode("cipher bytes"));
  return fs;
}

function core(pages: readonly string[][] = [[id]]): CheckpointCore {
  let attachmentPage = 0;
  return {
    invoke: async request => {
      if (request.command === "_worker_checkpoint_identity") return { wrappedIdentity: "opaque rust envelope" };
      const page = pages[attachmentPage++] ?? [];
      return { attachmentIds: page };
    },
  };
}

function loaded(database = encoder.encode("encrypted database")): LoadedCheckpoint {
  return {
    manifest: { version: 1, generation: 7, files: ["client.db"], wrappedCredentials: encoder.encode("opaque") },
    database,
    readCipher: async () => encoder.encode("unused"),
  };
}

describe("browser checkpoint filesystem", () => {
  it("captures only SQLCipher and core-referenced cipher metadata", async () => {
    const fs = seededFs();
    fs.writeFile(`/peppy/${cipher(coldId)}`, encoder.encode("orphan"));
    fs.mkdir("/peppy/plain");
    fs.writeFile("/peppy/plain/preview", encoder.encode("plaintext"));
    const captured = await captureFilesystemCheckpoint({ fs, core: core(), expectedGeneration: 3, previousFiles: [] });
    expect(captured.files.map(file => file.path)).toEqual(["client.db", cipher(id)]);
    expect(new TextDecoder().decode(captured.wrappedCredentials)).toBe("opaque rust envelope");
  });

  it("retains a cold core-owned cipher and drops a discarded reference", async () => {
    const fs = seededFs();
    const retained = await captureFilesystemCheckpoint({ fs, core: core([[coldId]]), expectedGeneration: 3, previousFiles: ["client.db", cipher(coldId)] });
    expect(retained.retainedCipherPaths).toEqual([cipher(coldId)]);
    const discarded = await captureFilesystemCheckpoint({ fs, core: core([[]]), expectedGeneration: 3, previousFiles: [cipher(coldId)] });
    expect(discarded.retainedCipherPaths).toEqual([]);
  });

  it("refuses hot journals, symlinks, traversal, malformed pages, and missing local cipher", async () => {
    const hot = seededFs();
    hot.writeFile("/peppy/client.db-wal", encoder.encode("hot"));
    await expect(captureFilesystemCheckpoint({ fs: hot, core: core(), expectedGeneration: 0, previousFiles: [] })).rejects.toMatchObject({ code: "invalid-checkpoint" });

    const linked = seededFs();
    linked.symlink("/peppy/client.db-journal");
    await expect(captureFilesystemCheckpoint({ fs: linked, core: core(), expectedGeneration: 0, previousFiles: [] })).rejects.toMatchObject({ code: "invalid-checkpoint" });
    const databaseLink = new MemoryFs();
    databaseLink.mkdir("/peppy");
    databaseLink.symlink("/peppy/client.db");
    await expect(captureFilesystemCheckpoint({ fs: databaseLink, core: core(), expectedGeneration: 0, previousFiles: [] })).rejects.toMatchObject({ code: "invalid-checkpoint" });
    await expect(captureFilesystemCheckpoint({ fs: seededFs(), core: core([["../escape"]]), expectedGeneration: 0, previousFiles: [] })).rejects.toMatchObject({ code: "invalid-checkpoint" });
    await expect(captureFilesystemCheckpoint({ fs: seededFs(), core: core([[coldId]]), expectedGeneration: 0, previousFiles: [] })).rejects.toMatchObject({ code: "invalid-checkpoint" });
  });

  it("refuses duplicate and zero-progress attachment pages", async () => {
    const full = Array.from({ length: 1_000 }, (_, index) => `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`);
    await expect(captureFilesystemCheckpoint({ fs: seededFs(), core: core([full, full]), expectedGeneration: 0, previousFiles: [] })).rejects.toMatchObject({ code: "invalid-checkpoint" });
  });

  it("does not replace an existing database during restore", () => {
    expect(() => restoreFilesystemCheckpoint(loaded(), seededFs())).toThrow(CheckpointError);
  });

  it("restores a fresh database and lazily hydrates canonical ciphertext at its generation", async () => {
    const fs = new MemoryFs();
    restoreFilesystemCheckpoint(loaded(), fs);
    expect(new TextDecoder().decode(fs.readFile("/peppy/client.db"))).toBe("encrypted database");
    let read: { path: string; generation: number } | undefined;
    await hydrateCipher(id, 7, { readCipher: async (path, generation) => {
      read = { path, generation };
      return encoder.encode("cipher bytes");
    } }, fs);
    expect(read).toEqual({ path: cipher(id), generation: 7 });
    expect(new TextDecoder().decode(fs.readFile(`/peppy/${cipher(id)}`))).toBe("cipher bytes");
  });

  it("checkpoints and hydrates a draft-only ciphertext at the committed generation", async () => {
    const draftId = "00000000-0000-4000-8000-000000000111";
    const fs = seededFs();
    fs.writeFile(`/peppy/${cipher(draftId)}`, encoder.encode("draft ciphertext"));
    const captured = await captureFilesystemCheckpoint({ fs, core: core([[draftId]]), expectedGeneration: 11, previousFiles: [] });
    expect(captured.files.map(file => file.path)).toEqual(["client.db", cipher(draftId)]);

    const restored = new MemoryFs();
    restoreFilesystemCheckpoint({ ...loaded(), manifest: { version: 1, generation: 11, files: ["client.db", cipher(draftId)], wrappedCredentials: encoder.encode("opaque") } }, restored);
    let generation = 0;
    await hydrateCipher(draftId, 11, { readCipher: async (_path, committedGeneration) => { generation = committedGeneration; return encoder.encode("draft ciphertext"); } }, restored);
    expect(generation).toBe(11);
  });

  it("fails rather than overwriting conflicting hydrated ciphertext", async () => {
    const fs = new MemoryFs();
    restoreFilesystemCheckpoint(loaded(), fs);
    fs.writeFile(`/peppy/${cipher(id)}`, encoder.encode("old"));
    await expect(hydrateCipher(id, 7, { readCipher: async () => encoder.encode("new") }, fs)).rejects.toMatchObject({ code: "invalid-checkpoint" });
  });

  it("retains a resident cipher already committed at the previous generation without copying it again", async () => {
    const fs = seededFs();
    const reads: string[] = [];
    const readFile = fs.readFile.bind(fs);
    fs.readFile = (path: string) => { reads.push(path); return readFile(path); };

    const captured = await captureFilesystemCheckpoint({ fs, core: core([[id]]), expectedGeneration: 4, previousFiles: ["client.db", cipher(id)] });

    expect(captured.files.map(file => file.path)).toEqual(["client.db"]);
    expect(captured.retainedCipherPaths).toEqual([cipher(id)]);
    expect(reads).toEqual(["/peppy/client.db"]);
  });

  it("drops a previously committed cipher whose attachment was removed from the manifest", async () => {
    const captured = await captureFilesystemCheckpoint({ fs: seededFs(), core: core([[]]), expectedGeneration: 4, previousFiles: ["client.db", cipher(id)] });

    expect(captured.files.map(file => file.path)).toEqual(["client.db"]);
    expect(captured.retainedCipherPaths).toEqual([]);
  });
});
