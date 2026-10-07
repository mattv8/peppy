import { describe, expect, it } from "vitest";
import { acquireWorkerOwner, OwnerError, type OwnerLockManager } from "./owner.js";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

function locks(): OwnerLockManager {
  let occupied = false;
  return {
    request: async (_name, options, operation) => {
      expect(options).toEqual({mode:"exclusive",ifAvailable:true});
      if (occupied) return operation(null);
      occupied = true;
      try { return await operation({name:"fixture"}); }
      finally { occupied = false; }
    },
  };
}

describe("browser worker ownership", () => {
  it("keeps another worker out until the previous runtime finishes stopping", async () => {
    const manager = locks();
    const stopped = deferred<void>();
    let secondStarted = false;
    const first = await acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>({id:1}),stop:async()=>stopped.promise});
    await expect(acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>{secondStarted=true;return {};},stop:async()=>undefined})).rejects.toMatchObject({code:"already-open"});
    expect(secondStarted).toBe(false);
    const released = first.release();
    await expect(acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>({}),stop:async()=>undefined})).rejects.toMatchObject({code:"already-open"});
    stopped.resolve();
    await released;
    const replacement = await acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>({id:2}),stop:async()=>undefined});
    expect(replacement.owner.id).toBe(2);
    await replacement.release();
  });

  it("aborts startup without publishing an owner and stops a late boot once", async () => {
    const manager=locks();
    const started=deferred<void>();
    const boot=deferred<{id:number}>();
    const cancelled=new AbortController();
    let stops=0;
    const pending=acquireWorkerOwner({locks:manager,name:"peppy-fixture",signal:cancelled.signal,start:async signal=>{started.resolve();await boot.promise;expect(signal.aborted).toBe(true);return {id:1};},stop:async()=>{stops++;}});
    await started.promise;
    cancelled.abort();
    boot.resolve({id:1});
    await expect(pending).rejects.toMatchObject({code:"cancelled"});
    expect(stops).toBe(1);
    const next=await acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>({}),stop:async()=>undefined});
    await next.release();
  });

  it("sanitizes startup failure and fails closed without Web Locks", async () => {
    const manager=locks();
    await expect(acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>{throw new Error("private startup data");},stop:async()=>undefined})).rejects.toMatchObject({code:"unavailable",message:"Browser worker ownership is unavailable."});
    await expect(acquireWorkerOwner({locks:undefined,name:"peppy-fixture",start:async()=>({}),stop:async()=>undefined})).rejects.toMatchObject({code:"unsupported"});
  });

  it("releases idempotently and never launches work for an already-aborted caller", async () => {
    const cancelled = new AbortController(); cancelled.abort();
    let starts=0;
    const manager=locks();
    await expect(acquireWorkerOwner({locks:manager,name:"peppy-fixture",signal:cancelled.signal,start:async()=>{starts++;return {};},stop:async()=>undefined})).rejects.toMatchObject({code:"cancelled"});
    expect(starts).toBe(0);
    let stops=0;
    const lease=await acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>({}),stop:async()=>{stops++;}});
    await Promise.all([lease.release(),lease.release()]);
    expect(stops).toBe(1);
    expect(OwnerError).toBeDefined();
  });

  it("keeps the lock held if teardown cannot establish that the old owner stopped", async () => {
    const manager=locks();
    const lease=await acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>({}),stop:async()=>{throw new Error("uncertain teardown");}});
    await expect(lease.release()).rejects.toMatchObject({code:"unavailable"});
    await expect(acquireWorkerOwner({locks:manager,name:"peppy-fixture",start:async()=>({}),stop:async()=>undefined})).rejects.toMatchObject({code:"already-open"});
  });
});
