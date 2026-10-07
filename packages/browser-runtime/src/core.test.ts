import { describe, expect, it } from "vitest";
import { BrowserCore, BrowserCoreError, CoreRejectedError, type EmscriptenCoreModule } from "./core.js";

function moduleWith(dispatch: (heap: Uint8Array) => number): EmscriptenCoreModule & { freed: number; responseFreed: number } {
  const heap = new Uint8Array(256);
  return { HEAPU8: heap, freed: 0, responseFreed: 0, _peppy_browser_alloc: () => 8, _peppy_browser_dispatch: () => dispatch(heap), _peppy_browser_free_request: () => undefined, _peppy_browser_free_response: () => undefined, UTF8ToString: () => "" };
}

describe("BrowserCore", () => {
  it("distinguishes a handled wrong phrase from a poisoned core or malformed response", async () => {
    const wasm=moduleWith(()=>100);
    const core=new BrowserCore(async()=>wasm);
    wasm.UTF8ToString=()=>JSON.stringify({ok:false,error:{code:"unlock-failed"}});
    await expect(core.invoke({command:"unlock",args:{}})).rejects.toBeInstanceOf(CoreRejectedError);
    for (const code of ["identity-exists", "identity-unavailable", "epoch-mismatch", "unavailable", "invalid-route", "mms-too-large"]) {
      wasm.UTF8ToString=()=>JSON.stringify({ok:false,error:{code}});
      await expect(core.invoke({command:"fixture",args:{}})).rejects.toBeInstanceOf(CoreRejectedError);
    }
    for (const response of ['not json',JSON.stringify({ok:false,error:{code:"core-poisoned"}})]) {
      wasm.UTF8ToString=()=>response;
      try { await core.invoke({command:"snapshot",args:{}}); throw new Error("expected failure"); }
      catch(error) { expect(error).toBeInstanceOf(BrowserCoreError); expect(error).not.toBeInstanceOf(CoreRejectedError); }
    }
  });
  it("keeps only canonical revision metadata from Rust errors", async () => {
    const wasm = moduleWith(() => 100);
    const core = new BrowserCore(async () => wasm);
    for (const currentRevision of ["secret-fixture", "01", "-1", "18446744073709551616"]) {
      wasm.UTF8ToString = () => JSON.stringify({ok:false,error:{code:"stale-draft",message:"secret-fixture",currentRevision}});
      try {
        await core.invoke({command:"save_draft",args:{}});
        throw new Error("expected rejection");
      } catch (error) {
        expect(error).toBeInstanceOf(BrowserCoreError);
        expect((error as BrowserCoreError).details?.currentRevision).toBeUndefined();
        expect((error as Error).message).not.toContain("secret-fixture");
      }
    }
    wasm.UTF8ToString = () => JSON.stringify({ok:false,error:{code:"stale-draft",currentRevision:"18446744073709551615"}});
    await expect(core.invoke({command:"save_draft",args:{}})).rejects.toMatchObject({code:"stale-draft",details:{currentRevision:"18446744073709551615"}});
  });
  it("keeps well-formed Rust rejections recoverable even when the UI does not recognize their code", async () => {
    const wasm = moduleWith(() => 100);
    wasm.UTF8ToString = () => JSON.stringify({ ok: false, error: { code: "contact-field-read-only" } });
    await expect(new BrowserCore(async () => wasm).invoke({ command: "submit_contact_edit", args: {} })).rejects.toMatchObject({ code: "contact-field-read-only" });
  });
  it("zeroizes and frees a request when dispatch fails", async () => {
    const wasm = moduleWith(() => { throw new Error("native failure"); });
    wasm._peppy_browser_free_request = () => { wasm.freed++; };
    await expect(new BrowserCore(async () => wasm).invoke({ command: "open", args: { secret: "not retained" } })).rejects.toBeInstanceOf(BrowserCoreError);
    expect(wasm.freed).toBe(1);
    expect([...wasm.HEAPU8.slice(8, 80)]).toEqual(Array(72).fill(0));
  });

  it("uses the current heap when Emscripten grows memory", async () => {
    const wasm = moduleWith(() => 100);
    wasm._peppy_browser_dispatch = () => { wasm.HEAPU8 = new Uint8Array(512); wasm.HEAPU8.set(new TextEncoder().encode('{"ok":true,"value":"ok"}\0'), 100); return 100; };
    wasm.UTF8ToString = (pointer) => {
      const end = wasm.HEAPU8.indexOf(0, pointer);
      return new TextDecoder().decode(wasm.HEAPU8.slice(pointer, end));
    };
    wasm._peppy_browser_free_request = () => { wasm.freed++; };
    wasm._peppy_browser_free_response = () => { wasm.responseFreed++; };
    await expect(new BrowserCore(async () => wasm).invoke({ command: "snapshot", args: {} })).resolves.toBe("ok");
    expect(wasm.freed).toBe(1);
    expect(wasm.responseFreed).toBe(1);
  });

  it("uses Rust value envelopes and initializes one shared module", async () => {
    let factoryCalls = 0;
    const wasm = moduleWith(() => 100);
    wasm._peppy_browser_dispatch = () => {
      wasm.HEAPU8.set(new TextEncoder().encode('{"ok":true,"value":{"revision":"1"}}\0'), 100);
      return 100;
    };
    wasm.UTF8ToString = () => '{"ok":true,"value":{"revision":"1"}}';
    const core = new BrowserCore(async () => {
      factoryCalls++;
      return wasm;
    });
    await expect(Promise.all([core.invoke({ command: "snapshot", args: {} }), core.invoke({ command: "snapshot", args: {} })])).resolves.toEqual([{ revision: "1" }, { revision: "1" }]);
    expect(factoryCalls).toBe(1);
  });
});
