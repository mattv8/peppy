import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { McpAuthorizationConsent } from "./McpAuthorizationConsent";
import { MCP_READ_LABEL, grantScopeLabels } from "./mcpScopes";
import { bridge } from "../bridge/bridge";

vi.mock("../bridge/bridge", { spy: true });

type AuthorizationView = Awaited<ReturnType<typeof bridge.mcp_authorization>>;
const authorization: AuthorizationView = { id: "request-1", clientName: "Claude", requestedScopes: ["conversations:read", "messages:send"], serverOrigin: "https://vault.example", resource: "https://vault.example/mcp", clientId: "client-1", redirectOrigin: "https://client.example", redirectIsLoopback: false };

function deferred<T>() { let resolve!: (value: T) => void; let reject!: (reason: unknown) => void; return { promise: new Promise<T>((complete, fail) => { resolve = complete; reject = fail; }), resolve, reject }; }

describe("McpAuthorizationConsent", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(bridge.mcp_authorization).mockResolvedValue(authorization);
    vi.mocked(bridge.mcp_status).mockResolvedValue({ vaultId: "11111111-1111-4111-8111-111111111111", available: true, serverOrigin: "https://vault.example", hosted: true, device: null });
    vi.mocked(bridge.mcp_approve_authorization).mockResolvedValue({ redirectUri: "https://client.example/callback?code=validated" });
    vi.mocked(bridge.mcp_deny_authorization).mockResolvedValue({ redirectUri: "https://client.example/callback?error=access_denied" });
    bridge.open_mcp_oauth_redirect = vi.fn().mockResolvedValue(undefined);
  });
  afterEach(cleanup);

  it("approves only a selected subset of stored requested scopes", async () => {
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    await screen.findByText("Claude");
    fireEvent.click(screen.getByRole("checkbox", { name: /Send real text messages/i }));
    fireEvent.click(screen.getByRole("button", { name: "Approve access" }));
    await waitFor(() => expect(bridge.mcp_approve_authorization).toHaveBeenCalledWith("request-1", ["conversations:read"]));
    expect(bridge.open_mcp_oauth_redirect).toHaveBeenCalledWith("https://client.example/callback?code=validated");
  });

  it("denies through the validated redirect", async () => {
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Deny access" }));
    await waitFor(() => expect(bridge.mcp_deny_authorization).toHaveBeenCalledWith("request-1"));
    expect(bridge.open_mcp_oauth_redirect).toHaveBeenCalledWith("https://client.example/callback?error=access_denied");
  });

  it("opens native OAuth callbacks through the dedicated bridge command", async () => {
    vi.mocked(bridge.mcp_approve_authorization).mockResolvedValueOnce({ redirectUri: "cursor://anysphere.cursor-retrieval/oauth/callback?code=validated" });
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve access" }));
    await waitFor(() => expect(bridge.open_mcp_oauth_redirect).toHaveBeenCalledWith("cursor://anysphere.cursor-retrieval/oauth/callback?code=validated"));
    expect(bridge.open_external_url).not.toHaveBeenCalled();
  });

  it("closes or retries an expired authorization fetch", async () => {
    vi.mocked(bridge.mcp_authorization).mockRejectedValueOnce(new Error("Authorization expired")).mockResolvedValueOnce({ id: "request-1", clientName: "Claude", requestedScopes: ["conversations:read"], serverOrigin: "https://vault.example", resource: "https://vault.example/mcp", clientId: "client-1", redirectOrigin: "https://client.example", redirectIsLoopback: false });
    const close = vi.fn();
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={close} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Authorization expired");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Claude")).toBeInTheDocument();
  });

  it("disables approval for an empty scope selection", async () => {
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("checkbox", { name: /Read conversations/i }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Send real text/i }));
    expect(screen.getByRole("button", { name: "Approve access" })).toBeDisabled();
  });

  it("retries only opening a consumed approval redirect", async () => {
    const opener = vi.fn().mockRejectedValueOnce(new Error("Opener blocked")).mockResolvedValueOnce(undefined);
    bridge.open_mcp_oauth_redirect = opener;
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve access" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Opener blocked");
    fireEvent.click(screen.getByRole("button", { name: "Retry opening redirect" }));
    await waitFor(() => expect(opener).toHaveBeenCalledTimes(2));
    expect(bridge.mcp_approve_authorization).toHaveBeenCalledTimes(1);
  });

  it("ignores a stale decision rejection after the authorization request changes", async () => {
    const decision = deferred<{ redirectUri: string }>();
    vi.mocked(bridge.mcp_approve_authorization).mockReturnValueOnce(decision.promise);
    const { rerender } = render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve access" }));
    rerender(<McpAuthorizationConsent authorizationId="request-2" browserHost={false} sessionKey="vault-b" focusFallback={{ current: null }} onClose={() => undefined} />);
    await act(async () => decision.reject(new Error("stale rejection")));
    await screen.findByText("Claude");
    expect(screen.queryByText("stale rejection")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve access" })).toBeEnabled();
  });

  it("does not navigate after an unmounted decision completes", async () => {
    const decision = deferred<{ redirectUri: string }>();
    vi.mocked(bridge.mcp_approve_authorization).mockReturnValueOnce(decision.promise);
    const { unmount } = render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve access" }));
    unmount();
    await act(async () => decision.resolve({ redirectUri: "https://client.example/callback?code=stale" }));
    expect(bridge.open_mcp_oauth_redirect).not.toHaveBeenCalled();
  });

  it("shows an immediate loading status with Close as the only action", () => {
    vi.mocked(bridge.mcp_authorization).mockReturnValue(deferred<AuthorizationView>().promise);
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading authorization request…");
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve access" })).not.toBeInTheDocument();
    expect(document.getElementById("mcp-authorization-body")).toHaveAttribute("data-mcp-authorization-state", "loading");
  });

  it("orders the ready copy with the name note, return target, client ID, scopes, and approval prompt last", async () => {
    const { container } = render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    await screen.findByText("Claude");
    const body = container.ownerDocument.getElementById("mcp-authorization-body");
    expect(body).toHaveAttribute("data-mcp-authorization-state", "ready");
    const ordered = ["#mcp-authorization-client", "#mcp-authorization-name-note", "#mcp-authorization-identity", "#mcp-authorization-client-id", "#mcp-scope-fieldset", "#mcp-authorization-recall-note", "#mcp-authorization-confirm-prompt"].map(selector => body?.querySelector(selector));
    ordered.forEach(element => expect(element).not.toBeNull());
    for (let index = 1; index < ordered.length; index += 1) {
      expect(ordered[index - 1]?.compareDocumentPosition(ordered[index] as Node) ?? 0).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    }
    expect(screen.getByText("Name provided by the requesting app.")).toBeInTheDocument();
    expect(screen.getByText("Anything the client copies outside Peppy can't be recalled.")).toBeInTheDocument();
    expect(screen.getByText("Approve only if you started this connection.")).toBeInTheDocument();
  });

  it("uses the same read label in the consent and grant lists", async () => {
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    expect(await screen.findByRole("checkbox", { name: MCP_READ_LABEL })).toBeInTheDocument();
    expect(grantScopeLabels["conversations:read"]).toBe(MCP_READ_LABEL);
  });

  it("marks a loopback return target as on this computer and an external one without the marker", async () => {
    vi.mocked(bridge.mcp_authorization).mockResolvedValueOnce({ ...authorization, redirectOrigin: "http://127.0.0.1:53682", redirectIsLoopback: true });
    const { unmount } = render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    await screen.findByText("Claude");
    expect(document.querySelector("[data-mcp-redirect-origin]")).toHaveTextContent("http://127.0.0.1:53682");
    expect(screen.getByText("on this computer")).toBeInTheDocument();
    unmount();

    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    await screen.findByText("Claude");
    expect(screen.queryByText("on this computer")).not.toBeInTheDocument();
  });

  it("keeps the client ID closed behind a disclosure", async () => {
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    await screen.findByText("Claude");
    const disclosure = document.getElementById("mcp-authorization-client-id") as HTMLDetailsElement;
    expect(disclosure.tagName).toBe("DETAILS");
    expect(disclosure.open).toBe(false);
    expect(within(disclosure).getByText("client-1")).toBeInTheDocument();
  });

  it("disables approval and explains the requirement when no scope is selected", async () => {
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("checkbox", { name: /Read conversations/i }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Send real text/i }));
    const approve = screen.getByRole("button", { name: "Approve access" });
    expect(approve).toBeDisabled();
    expect(approve).toHaveAttribute("aria-describedby", "mcp-scope-required");
    expect(screen.getByText("Select at least one permission to approve.")).toBeInTheDocument();
  });

  it("shows only Close and restart guidance for an expired authorization on load", async () => {
    vi.mocked(bridge.mcp_authorization).mockRejectedValueOnce({ code: "mcp_authorization_not_found", message: "not found" });
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    expect(await screen.findByText("This authorization request expired or was already used.")).toBeInTheDocument();
    expect(screen.getByText("Start the connection again in your AI client.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve access" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Deny access" })).not.toBeInTheDocument();
  });

  it("shows Close only for a native-shaped expired authorization load", async () => {
    vi.mocked(bridge.mcp_authorization).mockRejectedValueOnce({ code: "server-rejected", message: "The server rejected the request (HTTP 404, mcp_authorization_not_found)." });
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    expect(await screen.findByText("This authorization request expired or was already used.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
  });

  it("shows Close and Retry for an ordinary load error, and Retry loads the request", async () => {
    vi.mocked(bridge.mcp_authorization).mockRejectedValueOnce(new Error("Gateway unavailable")).mockResolvedValueOnce(authorization);
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Gateway unavailable");
    expect(screen.queryByText("expired or was already used")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Claude")).toBeInTheDocument();
  });

  it("shows the expired state with Close only when the decision reports the request is gone", async () => {
    vi.mocked(bridge.mcp_approve_authorization).mockRejectedValueOnce({ code: "mcp_authorization_not_found", message: "Authorization request is gone" });
    const onClose = vi.fn();
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={onClose} />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve access" }));
    expect(await screen.findByText("This authorization request expired or was already used.")).toBeInTheDocument();
    expect(screen.getByText("Start the connection again in your AI client.")).toBeInTheDocument();
    expect(screen.queryByText("Authorization request is gone")).not.toBeInTheDocument();
    expect(screen.queryByText("requests access to vault", { exact: false })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve access" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Deny access" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalled();
    expect(bridge.open_mcp_oauth_redirect).not.toHaveBeenCalled();
  });

  it.each([
    ["Approve access", "mcp_approve_authorization"],
    ["Deny access", "mcp_deny_authorization"],
  ])("shows Close only when native %s reports an expired authorization", async (action, method) => {
    const nativeExpired = { code: "server-rejected", message: "The server rejected the request (HTTP 404, mcp_authorization_not_found)." };
    if (method === "mcp_approve_authorization") vi.mocked(bridge.mcp_approve_authorization).mockRejectedValueOnce(nativeExpired);
    else vi.mocked(bridge.mcp_deny_authorization).mockRejectedValueOnce(nativeExpired);
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: action }));
    expect(await screen.findByText("This authorization request expired or was already used.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve access" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Deny access" })).not.toBeInTheDocument();
  });

  it("keeps Approve enabled after an ordinary decision error", async () => {
    vi.mocked(bridge.mcp_approve_authorization).mockRejectedValueOnce(new Error("Try again later"));
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve access" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Try again later");
    expect(screen.getByText("requests access to vault", { exact: false })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve access" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Deny access" })).toBeEnabled();
  });

  it.each([
    ["approve", "Couldn't approve this request. Try again."],
    ["deny", "Couldn't deny this request. Try again."],
  ])("shows native plain-object %s failures with the matching fallback", async (action, fallback) => {
    if (action === "approve") vi.mocked(bridge.mcp_approve_authorization).mockRejectedValueOnce({ code: "unknown", message: "" });
    else vi.mocked(bridge.mcp_deny_authorization).mockRejectedValueOnce({ code: "unknown", message: "" });
    render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: action === "approve" ? "Approve access" : "Deny access" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(fallback);
  });

  it("does not navigate for a stale decision completion after the authorization request changes", async () => {
    const decision = deferred<{ redirectUri: string }>();
    vi.mocked(bridge.mcp_approve_authorization).mockReturnValueOnce(decision.promise);
    const { rerender } = render(<McpAuthorizationConsent authorizationId="request-1" browserHost={false} sessionKey="vault-a" focusFallback={{ current: null }} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve access" }));
    rerender(<McpAuthorizationConsent authorizationId="request-2" browserHost={false} sessionKey="vault-b" focusFallback={{ current: null }} onClose={() => undefined} />);
    await act(async () => decision.resolve({ redirectUri: "https://client.example/callback?code=stale" }));
    expect(bridge.open_mcp_oauth_redirect).not.toHaveBeenCalled();
  });
});
