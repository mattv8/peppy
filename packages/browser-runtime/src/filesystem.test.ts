import { describe, expect, it } from "vitest";
import { CheckpointError, type LoadedCheckpoint } from "./checkpoint.js";
import { captureFilesystemCheckpoint, hydrateCipher, restoreFilesystemCheckpoint, type CheckpointCore, type EmscriptenFilesystem } from "./filesystem.js";

const encoder = new TextEncoder();
const id = "123e4567-e89b-12d3-a456-426614174000";
const coldId = "00000000-0000-4000-8000-000000000000";
const cipher = (attachmentId: string): string => `client.db.media/cipher/${attachmentId}.ppss`;

type Node = { kind: "directory"; children: Set<string> } | { kind: "file"; bytes: Uint8Array } | { kind: "link" };

class MemoryFs implements EmscriptenFilesystem {
  private readonly nodes = new Map<string, Node>([["/", { kind: "directory", children: new Set() }]]);

  public readdir(path: string): string[] {
    const node = this.node(path);
    if (node.kind !== "directory") throw new Error("not a directory");
    return [".", "..", ...node.children];
  }

  public lstat(path: string): { size: number; mode: number } {
    const node = this.node(path);
    return { mode: node.kind === "file" ? 1 : node.kind === "directory" ? 2 : 3, size: node.kind === "file" ? node.bytes.byteLength : 0 };
  }

  public isFile(mode: number): boolean { return mode === 1; }
  public isDir(mode: number): boolean { return mode === 2; }
  public isLink(mode: number): boolean { return mode === 3; }

  public readFile(path: string): Uint8Array {
    const node = this.node(path);
    if (node.kind !== "file") throw new Error("not a file");
    return new Uint8Array(node.bytes);
  }

  public writeFile(path: string, bytes: Uint8Array): void {
    if (this.nodes.has(path)) throw new Error("already exists");
    this.parent(path).children.add(this.name(path));
    this.nodes.set(path, { kind: "file", bytes: new Uint8Array(bytes) });
  }

  public mkdir(path: string): void {
    if (this.nodes.has(path)) throw new Error("already exists");
    this.parent(path).children.add(this.name(path));
    this.nodes.set(path, { kind: "directory", children: new Set() });
  }

  public unlink(path: string): void {
    this.parent(path).children.delete(this.name(path));
    if (!this.nodes.delete(path)) throw new Error("missing");
  }

  public symlink(path: string): void {
    this.parent(path).children.add(this.name(path));
    this.nodes.set(path, { kind: "link" });
  }

  private node(path: string): Node {
    const node = this.nodes.get(path);
    if (!node) throw new Error("missing");
    return node;
  }

  private parent(path: string): Extract<Node, { kind: "directory" }> {
    const parent = this.node(path.slice(0, path.lastIndexOf("/")) || "/");
    if (parent.kind !== "directory") throw new Error("parent is not directory");
    return parent;
  }

  private name(path: string): string { return path.slice(path.lastIndexOf("/") + 1); }
}

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

  it("fails rather than overwriting conflicting hydrated ciphertext", async () => {
    const fs = new MemoryFs();
    restoreFilesystemCheckpoint(loaded(), fs);
    fs.writeFile(`/peppy/${cipher(id)}`, encoder.encode("old"));
    await expect(hydrateCipher(id, 7, { readCipher: async () => encoder.encode("new") }, fs)).rejects.toMatchObject({ code: "invalid-checkpoint" });
  });
});
