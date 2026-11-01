import { describe, expect, it, vi } from "vitest";
import type { AttachmentRetentionRequestDto } from "@peppy/generated/attachment-retention";
import { AccountAccessDriver } from "./account-access.js";
import { TransportError } from "./transport.js";

const request: AttachmentRetentionRequestDto = {
  method: "GET", relativePath: "/v1/account/access", jsonBody: null, protocolHeader: null, expectedSuccessStatus: 200,
};

const notice = { kind: "notice", notice: { kind: "read_only_past_due", severity: "error", deadline: null, manageUrl: null, dismissible: false, snoozeKey: "read_only_past_due:none" } };

/** The core classifies status and body; this fake echoes what the Rust parse command would return for each status. */
function session(parse: (input: { status: number; body: string; trustedOrigins: string[] }) => unknown) {
  const queries: string[] = [];
  return {
    queries,
    async query(command: string, args: Record<string, unknown>) {
      queries.push(command);
      if (command === "_worker_account_access_request") return request;
      if (command === "_worker_account_access_parse") return parse(args as never);
      throw new Error(command);
    },
  };
}

const driver = (transport: { retention: (request: AttachmentRetentionRequestDto, signal: AbortSignal) => Promise<string> }, sessionFake: ReturnType<typeof session>) =>
  new AccountAccessDriver(sessionFake, transport, () => ["https://api.example.test", "https://peppy.pro"], () => 1, () => new AbortController().signal);

describe("AccountAccessDriver", () => {
  it("passes a 200 body and trusted origins to core and returns its notice", async () => {
    const fake = session(input => {
      expect(input.status).toBe(200);
      expect(input.body).toBe("{\"access\":\"read_only\"}");
      expect(input.trustedOrigins).toEqual(["https://api.example.test", "https://peppy.pro"]);
      return notice;
    });
    const result = await driver({ retention: vi.fn(async () => "{\"access\":\"read_only\"}") }, fake).status();
    expect(result).toEqual(notice);
    expect(fake.queries).toEqual(["_worker_account_access_request", "_worker_account_access_parse"]);
  });

  it("reports 404 and 501 from the transport as core-classified unsupported", async () => {
    for (const status of [404, 501]) {
      const fake = session(input => ({ kind: input.status === 404 || input.status === 501 ? "unsupported" : "none" }));
      const transport = { retention: vi.fn(async () => { throw new TransportError("status", status); }) };
      await expect(driver(transport, fake).status()).resolves.toEqual({ kind: "unsupported" });
    }
  });

  it("reports server failures and network errors as temporarily unavailable", async () => {
    const fake = session(input => ({ kind: input.status === 503 || input.status === 0 ? "temporarily_unavailable" : "none" }));
    await expect(driver({ retention: async () => { throw new TransportError("status", 503); } }, fake).status()).resolves.toEqual({ kind: "temporarily_unavailable" });
    await expect(driver({ retention: async () => { throw new TransportError("offline"); } }, fake).status()).resolves.toEqual({ kind: "temporarily_unavailable" });
  });

  it("returns 200 with no notice as none", async () => {
    const fake = session(() => ({ kind: "none" }));
    await expect(driver({ retention: async () => "{}" }, fake).status()).resolves.toEqual({ kind: "none" });
  });

  it("never returns a notice from an unrecognised core result", async () => {
    const fake = session(() => ({ kind: "bogus" }));
    await expect(driver({ retention: async () => "{}" }, fake).status()).resolves.toEqual({ kind: "temporarily_unavailable" });
  });

  it("does not report a result after the session generation changes", async () => {
    let generation = 1;
    const fake = session(() => notice);
    const pending = new AccountAccessDriver(fake, { retention: async () => { generation += 1; return "{}"; } }, () => [], () => generation, () => new AbortController().signal).status();
    await expect(pending).resolves.toEqual({ kind: "temporarily_unavailable" });
    expect(fake.queries).not.toContain("_worker_account_access_parse");
  });
});
