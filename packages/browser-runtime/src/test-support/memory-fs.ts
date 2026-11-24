import type { EmscriptenFilesystem } from "../filesystem.js";

type Node = { kind: "directory"; children: Set<string> } | { kind: "file"; bytes: Uint8Array } | { kind: "link" };

export class MemoryFs implements EmscriptenFilesystem {
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

  public seedDatabase(): void {
    this.mkdir("/peppy");
    this.writeFile("/peppy/client.db", new TextEncoder().encode("encrypted database"));
    this.mkdir("/peppy/client.db.media");
    this.mkdir("/peppy/client.db.media/cipher");
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
