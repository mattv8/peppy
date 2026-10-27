import { afterEach, describe, expect, it, vi } from "vitest";
import { AppSessionRenewal, armHostedSessionRenewal, restoreAfterPersistedPageShow } from "./app-session-renewal";

const accountId = "11111111-1111-7111-8111-111111111111";

function sessionResponse(signedIn = true): Response {
  return new Response(JSON.stringify(signedIn
    ? { signed_in: true, account_id: accountId, csrf_token: "csrf" }
    : { signed_in: false }));
}

function renewalResponse(body: unknown = { account_id: accountId, expires_in_seconds: 2_592_000 }, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

afterEach(() => vi.restoreAllMocks());

describe("AppSessionRenewal", () => {
  it("renews once after trusted visible interaction using the passive app-session CSRF", async () => {
    const fetch = vi.fn().mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse());
    const renewal = new AppSessionRenewal({ fetch, now: () => 0 });

    await renewal.onInteraction({ isTrusted: true });

    expect(fetch).toHaveBeenNthCalledWith(1, "/hosted/v1/web/app-session", { credentials: "include" });
    expect(fetch).toHaveBeenNthCalledWith(2, "/hosted/v1/web/app-session/renew", { method: "POST", credentials: "include", headers: { "X-Peppy-CSRF": "csrf" } });
  });

  it("does not renew for passive enrollment, untrusted, or hidden activity", async () => {
    const fetch = vi.fn();
    const renewal = new AppSessionRenewal({ fetch, now: () => 0, isVisible: () => false });

    await renewal.onInteraction({ isTrusted: false });
    await renewal.onInteraction({ isTrusted: true });

    expect(fetch).not.toHaveBeenCalled();
  });

  it("backs off signed-out and failed renewal attempts without clearing local state", async () => {
    let now = 0;
    const rejected = vi.fn();
    const fetch = vi.fn().mockResolvedValueOnce(sessionResponse(false)).mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse({}, 500));
    const renewal = new AppSessionRenewal({ fetch, now: () => now, onSessionRejected: rejected });

    await renewal.onInteraction({ isTrusted: true });
    await renewal.onInteraction({ isTrusted: true });
    now = 30_000;
    await renewal.onInteraction({ isTrusted: true });

    expect(fetch).toHaveBeenCalledTimes(3);
    expect(rejected).toHaveBeenCalledOnce();
  });

  it("refreshes enrollment only for definitive app-session rejection", async () => {
    const rejected = vi.fn();
    const fetch = vi.fn().mockResolvedValueOnce(renewalResponse({}, 401)).mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse({}, 500));
    const renewal = new AppSessionRenewal({ fetch, now: () => 0, onSessionRejected: rejected });

    await renewal.onInteraction({ isTrusted: true });
    await renewal.onInteraction({ isTrusted: true });

    expect(rejected).toHaveBeenCalledOnce();
  });

  it("rejects renewal responses with a wrong account, invalid TTL, or extra fields", async () => {
    for (const body of [
      { account_id: "22222222-2222-7222-8222-222222222222", expires_in_seconds: 2_592_000 },
      { account_id: accountId, expires_in_seconds: 0 },
      { account_id: accountId, expires_in_seconds: 2_592_001 },
      { account_id: accountId, expires_in_seconds: 2_592_000, credential: "unexpected" },
    ]) {
      const fetch = vi.fn().mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse(body));
      const renewal = new AppSessionRenewal({ fetch, now: () => 0 });
      await renewal.onInteraction({ isTrusted: true });
      await renewal.onInteraction({ isTrusted: true });
      expect(fetch).toHaveBeenCalledTimes(2);
    }
  });

  it("coalesces activity and keeps overlapping auth transitions suppressed", async () => {
    let resolveSession!: (response: Response) => void;
    const session = new Promise<Response>(resolve => { resolveSession = resolve; });
    const fetch = vi.fn().mockReturnValueOnce(session).mockResolvedValueOnce(renewalResponse());
    const renewal = new AppSessionRenewal({ fetch, now: () => 0 });
    const first = renewal.onInteraction({ isTrusted: true });
    const transition = vi.fn();
    const login = renewal.duringAuthTransition(transition);
    const logout = renewal.duringAuthTransition(transition);

    resolveSession(sessionResponse());
    await Promise.all([first, login, logout]);
    await renewal.onInteraction({ isTrusted: true });

    expect(fetch).toHaveBeenCalledTimes(2);
    expect(transition).toHaveBeenCalledTimes(2);
  });

  it("does not start an auth action when its in-flight renewal times out", async () => {
    const fetch = vi.fn(() => new Promise<Response>(() => undefined));
    const renewal = new AppSessionRenewal({ fetch, now: () => 0, transitionTimeoutMs: 0 });
    const action = vi.fn();
    void renewal.onInteraction({ isTrusted: true });

    await expect(renewal.duringAuthTransition(action)).rejects.toThrow("Session renewal is still in progress");

    expect(action).not.toHaveBeenCalled();
  });

  it("releases only terminal navigation suppression after a persisted pageshow", async () => {
    let now = 0;
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse())
      .mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse());
    const renewal = new AppSessionRenewal({ fetch, now: () => now });
    await renewal.duringAuthTransition(() => undefined, { persistsUntilUnload: true });
    await renewal.onInteraction({ isTrusted: true });
    restoreAfterPersistedPageShow({ persisted: true }, renewal);
    now = 900_000;
    await renewal.onInteraction({ isTrusted: true });

    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("keeps an active transition suppressed when persisted pageshow clears navigation suppression", async () => {
    let resolveAction!: () => void;
    const action = new Promise<void>(resolve => { resolveAction = resolve; });
    const fetch = vi.fn();
    const renewal = new AppSessionRenewal({ fetch, now: () => 0 });
    const transition = renewal.duringAuthTransition(() => action);
    await vi.waitFor(() => expect(resolveAction).toBeTypeOf("function"));

    restoreAfterPersistedPageShow({ persisted: true }, renewal);
    await renewal.onInteraction({ isTrusted: true });
    resolveAction();
    await transition;

    expect(fetch).not.toHaveBeenCalled();
  });

  it("registers trusted interaction listeners in capture phase without scroll", () => {
    const addEventListener = vi.spyOn(window, "addEventListener");
    const removeEventListener = vi.spyOn(window, "removeEventListener");
    const renewal = new AppSessionRenewal({ fetch: vi.fn(), now: () => 0 });

    const dispose = renewal.arm();

    expect(addEventListener).toHaveBeenCalledWith("pointerdown", expect.any(Function), { capture: true, passive: true });
    expect(addEventListener).toHaveBeenCalledWith("keydown", expect.any(Function), { capture: true, passive: true });
    expect(addEventListener).toHaveBeenCalledWith("touchstart", expect.any(Function), { capture: true, passive: true });
    expect(addEventListener).toHaveBeenCalledWith("wheel", expect.any(Function), { capture: true, passive: true });
    expect(addEventListener).not.toHaveBeenCalledWith("scroll", expect.any(Function), expect.anything());
    dispose();
    expect(removeEventListener).toHaveBeenCalledWith("pointerdown", expect.any(Function), true);
    expect(removeEventListener).toHaveBeenCalledWith("keydown", expect.any(Function), true);
    expect(removeEventListener).toHaveBeenCalledWith("touchstart", expect.any(Function), true);
    expect(removeEventListener).toHaveBeenCalledWith("wheel", expect.any(Function), true);
  });

  it("backs off when session rejection handling fails", async () => {
    let now = 0;
    const rejected = vi.fn(() => Promise.reject(new Error("projection failure")));
    const fetch = vi.fn().mockResolvedValueOnce(sessionResponse(false));
    const renewal = new AppSessionRenewal({ fetch, now: () => now, onSessionRejected: rejected });

    await expect(renewal.onInteraction({ isTrusted: true })).resolves.toBeUndefined();
    now = 29_999;
    await renewal.onInteraction({ isTrusted: true });

    expect(rejected).toHaveBeenCalledOnce();
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("throttles successful renewals for exactly fifteen minutes", async () => {
    let now = 0;
    const fetch = vi.fn().mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse()).mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(renewalResponse());
    const renewal = new AppSessionRenewal({ fetch, now: () => now });
    await renewal.onInteraction({ isTrusted: true });
    now = 899_999;
    await renewal.onInteraction({ isTrusted: true });
    now = 900_000;
    await renewal.onInteraction({ isTrusted: true });
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it("arms only when hosted configuration is available", () => {
    const arm = vi.fn(() => () => undefined);
    expect(armHostedSessionRenewal(false, { arm } as unknown as AppSessionRenewal)).toBeUndefined();
    expect(arm).not.toHaveBeenCalled();
    expect(armHostedSessionRenewal(true, { arm } as unknown as AppSessionRenewal)).toBeTypeOf("function");
    expect(arm).toHaveBeenCalledOnce();
  });
});
