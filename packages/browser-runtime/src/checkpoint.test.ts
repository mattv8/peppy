import "fake-indexeddb/auto";
import { describe, expect, it } from "vitest";
import { CheckpointError, IndexedDbCheckpointStore, RuntimeCoreError, SerializedRuntime, type CheckpointInput } from "./checkpoint.js";
import { BrowserCoreError, CoreRejectedError } from "./core.js";

const encoder = new TextEncoder();
const media = "client.db.media/cipher/123e4567-e89b-12d3-a456-426614174000.ppss";
const input = (generation: number, bytes = encoder.encode("encrypted database")): CheckpointInput => ({ expectedGeneration: generation, wrappedCredentials: encoder.encode("opaque ciphertext"), files: [{ path: "client.db", bytes }, { path: media, bytes: encoder.encode("ciphertext") }] });

async function alterStore(name: string, alter: (store: IDBObjectStore) => void): Promise<void> {
  const database = await new Promise<IDBDatabase>((resolve, reject) => {
    const open = indexedDB.open(name, 1);
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error);
  });
  await new Promise<void>((resolve, reject) => {
    const transaction = database.transaction("checkpoint", "readwrite");
    transaction.oncomplete = () => resolve();
    transaction.onabort = transaction.onerror = () => reject(transaction.error);
    alter(transaction.objectStore("checkpoint"));
  });
  database.close();
}

describe("IndexedDbCheckpointStore", () => {
  it("commits a copied encrypted generation and lazily reads referenced cipher", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    const checkpoint = input(0);
    await expect(store.commit(checkpoint)).resolves.toBe(1);
    checkpoint.files[0].bytes.fill(9);
    const loaded = await store.load(1);
    expect(loaded.kind).toBe("present");
    if (loaded.kind === "present") {
      expect(new TextDecoder().decode(loaded.checkpoint.database)).toBe("encrypted database");
      expect(new TextDecoder().decode(await loaded.checkpoint.readCipher(media))).toBe("ciphertext");
      await expect(loaded.checkpoint.readCipher("client.db.media/cipher/not-a-uuid.ppss")).rejects.toBeInstanceOf(CheckpointError);
    }
  });

  it("rejects plaintext and leaves no manifest", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    await expect(store.commit(input(0, encoder.encode("SQLite format 3\0not encrypted")))).rejects.toMatchObject({ code: "invalid-checkpoint" });
    await expect(store.load()).resolves.toEqual({ kind: "absent" });
  });

  it("uses its generation as a compare-and-swap fence", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    await store.commit(input(0));
    await expect(store.commit(input(0))).rejects.toMatchObject({ code: "storage-conflict" });
    const loaded = await store.load(1);
    expect(loaded.kind).toBe("present");
  });

  it("lets only one simultaneous same-generation commit win", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    const outcomes = await Promise.allSettled([store.commit(input(0)), store.commit(input(0))]);
    expect(outcomes.filter((outcome) => outcome.status === "fulfilled")).toHaveLength(1);
    expect(outcomes.filter((outcome) => outcome.status === "rejected")).toHaveLength(1);
  });

  it("rejects a lazy reader from an older committed generation", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    await store.commit(input(0));
    const loaded = await store.load(1);
    if (loaded.kind !== "present") throw new Error("checkpoint missing");
    await store.commit(input(1));
    await expect(loaded.checkpoint.readCipher(media)).rejects.toMatchObject({ code: "storage-conflict" });
  });

  it("does not let returned metadata retarget a lazy reader to another generation", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    await store.commit(input(0));
    const loaded = await store.load();
    if (loaded.kind !== "present") throw new Error("checkpoint missing");
    await store.commit(input(1));
    loaded.checkpoint.manifest.generation = 2;
    await expect(loaded.checkpoint.readCipher(media)).rejects.toMatchObject({code:"storage-conflict"});
  });

  it("retains explicitly referenced ciphertext without loading it into the worker", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    await store.commit(input(0));
    const next = input(1);
    next.files = [next.files[0]];
    next.retainedCipherPaths = [media];
    await store.commit(next);
    expect(new TextDecoder().decode(await store.readCipher(media, 2))).toBe("ciphertext");
    const saved = await store.load(2);
    if (saved.kind !== "present") throw new Error("checkpoint missing");
    expect(new TextDecoder().decode(await saved.checkpoint.readCipher(media))).toBe("ciphertext");
    const missing = "client.db.media/cipher/00000000-0000-4000-8000-000000000000.ppss";
    await expect(store.commit({...next,expectedGeneration:2,retainedCipherPaths:[missing]})).rejects.toMatchObject({code:"invalid-checkpoint"});
    await expect(store.load(2)).resolves.toMatchObject({kind:"present"});
  });

  it("rejects a disallowed file before writing anything", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    const base = input(0);
    const invalid: CheckpointInput = { ...base, files: [base.files[0], { path: "client.db.media/plain/photo", bytes: encoder.encode("no") }] };
    await expect(store.commit(invalid)).rejects.toMatchObject({ code: "invalid-checkpoint" });
    await expect(store.load()).resolves.toEqual({ kind: "absent" });
  });

  it("captures the expected generation before opening IndexedDB", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    const checkpoint = input(0);
    const pending = store.commit(checkpoint);
    checkpoint.expectedGeneration = 99;
    await expect(pending).resolves.toBe(1);
  });

  it("aborts an actual transaction when a later put throws", async () => {
    const store = new IndexedDbCheckpointStore(`test-${crypto.randomUUID()}`);
    await store.commit(input(0));
    const originalPut = IDBObjectStore.prototype.put;
    let puts = 0;
    IDBObjectStore.prototype.put = function (...arguments_: Parameters<IDBObjectStore["put"]>) {
      puts++;
      if (puts === 2) throw new DOMException("clone failure", "DataCloneError");
      return originalPut.apply(this, arguments_);
    };
    try {
      await expect(store.commit(input(1))).rejects.toMatchObject({ code: "storage-unavailable" });
    } finally {
      IDBObjectStore.prototype.put = originalPut;
    }
    const loaded = await store.load(1);
    expect(loaded.kind).toBe("present");
  });

  it("fails closed for a missing database blob or malformed manifest", async () => {
    const missingName = `test-${crypto.randomUUID()}`;
    const missing = new IndexedDbCheckpointStore(missingName);
    await missing.commit(input(0));
    missing.close();
    await alterStore(missingName, (objectStore) => objectStore.delete("file:client.db"));
    await expect(missing.load()).rejects.toMatchObject({ code: "invalid-checkpoint" });

    const malformedName = `test-${crypto.randomUUID()}`;
    const malformed = new IndexedDbCheckpointStore(malformedName);
    await malformed.commit(input(0));
    malformed.close();
    await alterStore(malformedName, (objectStore) => objectStore.put({ version: 2 }, "manifest"));
    await expect(malformed.load()).rejects.toMatchObject({ code: "invalid-checkpoint" });
  });

  it("rejects generation overflow without replacing the committed checkpoint", async () => {
    const name = `test-${crypto.randomUUID()}`;
    const store = new IndexedDbCheckpointStore(name);
    await store.commit(input(0));
    store.close();
    await alterStore(name, (objectStore) => objectStore.put({
      version: 1,
      generation: Number.MAX_SAFE_INTEGER,
      files: ["client.db"],
      wrappedCredentials: encoder.encode("opaque").buffer,
    }, "manifest"));
    await expect(store.commit(input(Number.MAX_SAFE_INTEGER))).rejects.toMatchObject({ code: "invalid-checkpoint" });
  });
});

describe("SerializedRuntime", () => {
  it("poisons a trapped read but keeps handled read rejections usable", async () => {
    let terminated=0;
    const runtime=new SerializedRuntime({commit:async()=>1},()=>{terminated++;});
    await expect(runtime.read(async()=>{throw new CoreRejectedError("not-found");})).rejects.toMatchObject({code:"not-found"});
    await expect(runtime.read(async()=>"still ready")).resolves.toBe("still ready");
    const trapped=runtime.read(async()=>{throw new WebAssembly.RuntimeError("fixture read trap");});
    const next=runtime.read(async()=>"must not run");
    await expect(trapped).rejects.toBeInstanceOf(RuntimeCoreError);
    await expect(next).rejects.toMatchObject({code:"storage-unavailable"});
    expect(terminated).toBe(1);
  });
  it("preserves a sanitized draft conflict only after its checkpoint settles", async () => {
    let commit!: (generation:number) => void;
    const completion = new Promise<number>(resolve=>{commit=resolve;});
    const conflict = new CoreRejectedError("stale-draft",{currentRevision:"9"});
    let committed = 0;
    const runtime = new SerializedRuntime({commit:()=>completion.then(()=>++committed)},()=>undefined);
    let settled = false;
    const pending = runtime.mutate({invoke:async()=>{throw conflict;},capture:async(generation)=>input(generation)});
    const checked = expect(pending).rejects.toBe(conflict);
    void pending.then(()=>{settled=true;},()=>{settled=true;});
    await Promise.resolve();
    expect(settled).toBe(false);
    commit(1);
    await checked;
    await expect(runtime.mutate({invoke:async()=>"next",capture:async(generation)=>{
      expect(generation).toBe(1);
      return input(generation);
    }})).resolves.toMatchObject({value:"next",generation:2});
  });
  it("does not run queued work after failed durable commit and terminates once", async () => {
    let calls = 0;
    let terminated = 0;
    const runtime = new SerializedRuntime({ commit: async () => { throw new CheckpointError("storage-unavailable"); } }, () => { terminated++; });
    const first = runtime.mutate({ invoke: async () => { calls++; return "sent"; }, capture: async (generation) => input(generation) });
    const second = runtime.mutate({ invoke: async () => { calls++; return "must not run"; }, capture: async (generation) => input(generation) });
    await expect(first).rejects.toMatchObject({ code: "storage-unavailable" });
    await expect(second).rejects.toMatchObject({ code: "storage-unavailable" });
    expect(calls).toBe(1);
    expect(terminated).toBe(1);
  });

  it("commits a restore guard after a mutating core error", async () => {
    let committed = 0;
    const runtime = new SerializedRuntime({ commit: async () => { committed++; return 1; } }, () => undefined);
    await expect(runtime.mutate({ invoke: async () => { throw new CoreRejectedError("core"); }, capture: async (generation) => input(generation) })).rejects.toBeInstanceOf(CoreRejectedError);
    expect(committed).toBe(1);
  });

  it("terminates a trapped owner without capturing or committing its filesystem", async () => {
    for (const fault of [new WebAssembly.RuntimeError("fixture trap"), 0, new BrowserCoreError("core-error")]) {
      let captures=0;
      let commits=0;
      let terminated=0;
      let queuedReads=0;
      const runtime=new SerializedRuntime({commit:async()=>{commits++;return 1;}},()=>{terminated++;});
      const failed=runtime.mutate({invoke:async()=>{throw fault;},capture:async generation=>{captures++;return input(generation);}});
      const queued=runtime.read(async()=>{queuedReads++;});
      await expect(failed).rejects.toBeInstanceOf(Error);
      await expect(queued).rejects.toMatchObject({code:"storage-unavailable"});
      expect({captures,commits,terminated,queuedReads}).toEqual({captures:0,commits:0,terminated:1,queuedReads:0});
    }
  });
});
