import { describe, expect, it, vi } from "vitest";
import { OriginTransport, TransportError } from "./transport.js";

const token = "a".repeat(96);

function streamedResponse(chunks: Uint8Array[], init: ResponseInit = {}, cancelError?: Error, closeAfterChunks = false): Response {
  let cancelled = false;
  let nextChunk = 0;
  const body = new ReadableStream<Uint8Array>({
    pull(controller) {
      const chunk = chunks[nextChunk++];
      if (chunk === undefined) {
        if (closeAfterChunks) controller.close();
        else return new Promise<void>(() => undefined);
        return;
      }
      controller.enqueue(chunk);
    },
    cancel() {
      cancelled = true;
      if (cancelError !== undefined) throw cancelError;
    },
  });
  const response = new Response(body, init);
  Object.defineProperty(response, "wasCancelled", { get: () => cancelled });
  return response;
}

describe("OriginTransport", () => {
  it("calls the default Worker fetch with the global receiver", async () => {
    const originalFetch = globalThis.fetch;
    let receiver: unknown;
    globalThis.fetch = function(this: unknown) {
      receiver = this;
      return Promise.resolve(new Response("{}"));
    } as typeof fetch;
    try {
      const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token });
      await transport.json("/v1/status", { method: "GET" });
      expect(receiver).toBe(globalThis);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
  it("uses canonical device-authenticated fetch options", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response('{"ok":true}'));
    const transport = new OriginTransport({ origin: "https://api.example.test/", deviceToken: () => token, fetch: fetcher });
    await expect(transport.json<{ ok: boolean }>("/v1/status?cursor=1", { method: "GET" })).resolves.toEqual({ ok: true });
    const [url, init] = fetcher.mock.calls[0];
    expect(url).toBe("https://api.example.test/v1/status?cursor=1");
    expect(init).toMatchObject({ method: "GET", redirect: "error", credentials: "omit", cache: "no-store", referrerPolicy: "no-referrer" });
    const headers = init?.headers as Headers;
    expect(headers.get("Authorization")).toBe(`Bearer ${token}`);
    expect(headers.get("Content-Type")).toBeNull();
  });

  it("refuses untrusted URLs before fetch", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: fetcher });
    await expect(transport.json("https://evil.test/v1/x", { method: "GET" })).rejects.toMatchObject({ kind: "invalid" });
    await expect(transport.json("/v1/x?token=secret", { method: "GET" })).rejects.toMatchObject({ kind: "invalid" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("bounds streamed bodies independently of content length and cancels overflow", async () => {
    const response = streamedResponse([new Uint8Array(4), new Uint8Array(4)], { headers: { "content-length": "1" } }) as Response & { wasCancelled: boolean };
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: vi.fn<typeof fetch>().mockResolvedValue(response) });
    await expect(transport.download("/v1/attachment", { limit: 6 })).rejects.toMatchObject({ kind: "too-large" });
    expect(response.wasCancelled).toBe(true);
  });

  it("maps malformed JSON, revocation, and unsafe error codes without exposing secrets", async () => {
    const malformed = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: vi.fn<typeof fetch>().mockResolvedValue(new Response("<html>")) });
    await expect(malformed.json("/v1/x", { method: "GET" })).rejects.toMatchObject({ kind: "invalid" });
    const revoked = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: vi.fn<typeof fetch>().mockResolvedValue(new Response("secret", { status: 401 })) });
    await expect(revoked.json("/v1/x", { method: "GET" })).rejects.toMatchObject({ kind: "revoked" });
    const rejected = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: vi.fn<typeof fetch>().mockResolvedValue(new Response('{"code":"<script>secret</script>"}', { status: 400 })) });
    await expect(rejected.json("/v1/x", { method: "GET" })).rejects.toEqual(expect.objectContaining({ kind: "status", status: 400, code: undefined }));
    await expect(rejected.json("/v1/x", { method: "GET" })).rejects.not.toThrow("secret");
  });

  it("does not retry uploads and sanitizes token validation errors", async () => {
    const fetcher = vi.fn<typeof fetch>().mockRejectedValue(new Error("network secret"));
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: fetcher });
    await expect(transport.upload("/v1/attachment", new Uint8Array([1]))).rejects.toMatchObject({ kind: "offline" });
    expect(fetcher).toHaveBeenCalledTimes(1);
    const invalidToken = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => "secret", fetch: fetcher });
    await expect(invalidToken.download("/v1/x", { limit: 1 })).rejects.toEqual(expect.objectContaining({ kind: "invalid", message: "Transport request failed" }));
  });

  it("posts public derivatives as authenticated binary without JSON encoding", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response('{"token":"share","safe_name":"image.png"}'));
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: fetcher });
    await expect(transport.postPublicCopy<{ token: string }>("/v1/attachments/remote/public-copies", new Uint8Array([1, 2]), { fileName: "image.png" })).resolves.toEqual({ token: "share", safe_name: "image.png" });
    const [, init] = fetcher.mock.calls[0];
    expect(init?.method).toBe("POST");
    expect(init?.body).toEqual(new Uint8Array([1, 2]));
    expect((init?.headers as Headers).get("Content-Type")).toBe("application/octet-stream");
    expect((init?.headers as Headers).get("X-File-Name")).toBe("image.png");
  });

  it("aborts requests from the caller", async () => {
    const fetcher = vi.fn<typeof fetch>((_url, init) => new Promise((_resolve, reject) => init?.signal?.addEventListener("abort", () => reject(new DOMException("", "AbortError")))));
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: fetcher });
    const controller = new AbortController();
    const pending = transport.json("/v1/x", { method: "GET", signal: controller.signal });
    controller.abort();
    await expect(pending).rejects.toMatchObject({ kind: "cancelled" });
  });

  it("aborts an expired JSON deadline and clears its timer", async () => {
    vi.useFakeTimers();
    try {
      const fetcher = vi.fn<typeof fetch>((_url, init) => new Promise((_resolve, reject) => init?.signal?.addEventListener("abort", () => reject(new DOMException("", "AbortError")))));
      const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: fetcher });
      const pending = transport.json("/v1/x", { method: "GET" });
      const outcome = expect(pending).rejects.toMatchObject({ kind: "offline" });
      await vi.advanceTimersByTimeAsync(20_000);
      await outcome;
      expect(vi.getTimerCount()).toBe(0);
    } finally { vi.useRealTimers(); }
  });
  it("accepts uppercase native device tokens without transforming the header", async () => {
    const uppercaseToken = "A1".repeat(48);
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response("{}"));
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => uppercaseToken, fetch: fetcher });

    await transport.json("/v1/status", { method: "GET" });

    const headers = fetcher.mock.calls[0][1]?.headers as Headers;
    expect(headers.get("Authorization")).toBe(`Bearer ${uppercaseToken}`);
  });

  it("rejects API-prefix escapes and encoded backslashes before credentials or fetch", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const deviceToken = vi.fn(() => token);
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken, fetch: fetcher });

    for (const path of ["/v1/../outside", "/v1/%2e%2e/outside", "/v1/%5coutside", "/v1/..%5coutside"]) {
      await expect(transport.json(path, { method: "GET" })).rejects.toMatchObject({ kind: "invalid" });
    }

    expect(deviceToken).not.toHaveBeenCalled();
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("rejects JSON request bodies larger than the C ABI page bound", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: fetcher });
    const oversizedBody = { value: "x".repeat(8 * 1024 * 1024) };

    await expect(transport.json("/v1/upload", { method: "POST", body: oversizedBody })).rejects.toMatchObject({ kind: "too-large" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("sanitizes credential-provider exceptions", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const transport = new OriginTransport({
      origin: "https://api.example.test",
      deviceToken: () => { throw new Error("raw-device-secret"); },
      fetch: fetcher,
    });

    await expect(transport.json("/v1/status", { method: "GET" })).rejects.toEqual(
      expect.objectContaining({ kind: "invalid", message: "Transport request failed" }),
    );
    await expect(transport.json("/v1/status", { method: "GET" })).rejects.not.toThrow("raw-device-secret");
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("rejects an already-aborted caller without credentials, fetch, or timers", async () => {
    vi.useFakeTimers();
    try {
      const fetcher = vi.fn<typeof fetch>();
      const deviceToken = vi.fn(() => token);
      const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken, fetch: fetcher });
      const controller = new AbortController();
      controller.abort();

      await expect(transport.json("/v1/status", { method: "GET", signal: controller.signal })).rejects.toMatchObject({ kind: "cancelled" });
      expect(deviceToken).not.toHaveBeenCalled();
      expect(fetcher).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the transfer deadline active while reading the response body", async () => {
    vi.useFakeTimers();
    let bodyController: ReadableStreamDefaultController<Uint8Array> | undefined;
    let requestSignal: AbortSignal | undefined;
    let pending: Promise<Uint8Array> | undefined;
    try {
      const fetcher = vi.fn<typeof fetch>((_url, init) => {
        requestSignal = init?.signal ?? undefined;
        const body = new ReadableStream<Uint8Array>({
          start(controller) {
            bodyController = controller;
            requestSignal?.addEventListener("abort", () => controller.error(new DOMException("", "AbortError")), { once: true });
          },
        });
        return Promise.resolve(new Response(body));
      });
      const transport = new OriginTransport({ origin: "https://api.example.test", deviceToken: () => token, fetch: fetcher });
      pending = transport.download("/v1/attachment", { limit: 16 });
      const outcome = expect(pending).rejects.toMatchObject({ kind: "offline" });
      await vi.advanceTimersByTimeAsync(30_000);

      expect(requestSignal?.aborted).toBe(true);
      await outcome;
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      if (requestSignal?.aborted !== true) bodyController?.error(new DOMException("", "AbortError"));
      await pending?.catch(() => undefined);
      vi.useRealTimers();
    }
  });

  it("cancels rejected response bodies without replacing the transport error", async () => {
    const contentLengthResponse = streamedResponse(
      [new Uint8Array(1)],
      { headers: { "content-length": "17" } },
      new Error("cancel-secret"),
    ) as Response & { wasCancelled: boolean };
    const tooLarge = new OriginTransport({
      origin: "https://api.example.test",
      deviceToken: () => token,
      fetch: vi.fn<typeof fetch>().mockResolvedValue(contentLengthResponse),
    });

    await expect(tooLarge.download("/v1/attachment", { limit: 16 })).rejects.toMatchObject({ kind: "too-large" });
    expect(contentLengthResponse.wasCancelled).toBe(true);
    expect(contentLengthResponse.body?.locked).toBe(false);

    const revokedResponse = streamedResponse([new TextEncoder().encode("secret")], { status: 401 }) as Response & { wasCancelled: boolean };
    const revoked = new OriginTransport({
      origin: "https://api.example.test",
      deviceToken: () => token,
      fetch: vi.fn<typeof fetch>().mockResolvedValue(revokedResponse),
    });

    await expect(revoked.json("/v1/status", { method: "GET" })).rejects.toMatchObject({ kind: "revoked" });
    expect(revokedResponse.wasCancelled).toBe(true);
    expect(revokedResponse.body?.locked).toBe(false);
  });

  it("keeps oversized error bodies and malformed JSON sanitized and unlocked", async () => {
    const oversizedError = streamedResponse(
      [new TextEncoder().encode('{"code":"must_not_escape"}')],
      { status: 400, headers: { "content-length": "4097" } },
      new Error("cancel-secret"),
    ) as Response & { wasCancelled: boolean };
    const rejected = new OriginTransport({
      origin: "https://api.example.test",
      deviceToken: () => token,
      fetch: vi.fn<typeof fetch>().mockResolvedValue(oversizedError),
    });

    await expect(rejected.json("/v1/status", { method: "GET" })).rejects.toEqual(
      expect.objectContaining({ kind: "status", status: 400, code: undefined, message: "Transport request failed" }),
    );
    expect(oversizedError.wasCancelled).toBe(true);
    expect(oversizedError.body?.locked).toBe(false);

    const malformedResponse = new Response("<html>");
    const malformed = new OriginTransport({
      origin: "https://api.example.test",
      deviceToken: () => token,
      fetch: vi.fn<typeof fetch>().mockResolvedValue(malformedResponse),
    });
    await expect(malformed.json("/v1/status", { method: "GET" })).rejects.toEqual(
      expect.objectContaining({ kind: "invalid", message: "Transport request failed" }),
    );
    expect(malformedResponse.body?.locked).toBe(false);
  });

  it("rejects and cancels unexpected same-origin response URLs", async () => {
    const response = streamedResponse([new Uint8Array([1])], {}, undefined, true) as Response & { wasCancelled: boolean };
    Object.defineProperty(response, "url", { value: "https://api.example.test/outside" });
    const transport = new OriginTransport({
      origin: "https://api.example.test",
      deviceToken: () => token,
      fetch: vi.fn<typeof fetch>().mockResolvedValue(response),
    });

    await expect(transport.download("/v1/attachment", { limit: 16 })).rejects.toMatchObject({ kind: "invalid" });
    expect(response.wasCancelled).toBe(true);
    expect(response.body?.locked).toBe(false);
  });

});
