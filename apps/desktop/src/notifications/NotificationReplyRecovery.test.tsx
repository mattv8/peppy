import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { NotificationReplyRecovery } from "./NotificationReplyRecovery";

afterEach(cleanup);

const recovery = { id: "reply-1", conversationId: "conversation-1", body: "A reply", reason: "needs-route-choice", state: "ready" as const };

const bridge = () => ({
  load: vi.fn().mockResolvedValue({ sessionGeneration: "generation-1", items: [recovery] }),
  claim: vi.fn().mockResolvedValue(true),
  release: vi.fn().mockResolvedValue(undefined),
  acknowledge: vi.fn().mockResolvedValue(undefined),
  discard: vi.fn().mockResolvedValue(undefined),
});

it("adds a claimed reply once and preserves the existing draft text", async () => {
  const native = bridge();
  const append = vi.fn().mockResolvedValue(undefined);
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-1" unlocked draftText="Existing draft" onAppend={append} />);
  fireEvent.click(await screen.findByRole("button", { name: "Unsent replies (1)" }));
  fireEvent.click(screen.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(append).toHaveBeenCalledWith("A reply"));
  expect(native.claim).toHaveBeenCalledTimes(1);
  expect(native.acknowledge).toHaveBeenCalledTimes(1);
});

it("retries only acknowledgement after an acknowledgement failure", async () => {
  const native = bridge();
  native.acknowledge.mockRejectedValueOnce(new Error("lost"));
  const append = vi.fn().mockResolvedValue(undefined);
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-1" unlocked draftText="" onAppend={append} />);
  fireEvent.click(await screen.findByRole("button", { name: "Unsent replies (1)" }));
  fireEvent.click(screen.getByRole("button", { name: "Add to draft" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not finish removing");
  fireEvent.click(screen.getByRole("button", { name: "Finish removing reply" }));
  await waitFor(() => expect(native.acknowledge).toHaveBeenCalledTimes(2));
  expect(append).toHaveBeenCalledTimes(1);
});

it("does not claim an overflowing combined draft", async () => {
  const native = bridge();
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-1" unlocked draftText={"x".repeat(64 * 1024)} onAppend={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "Unsent replies (1)" }));
  fireEvent.click(screen.getByRole("button", { name: "Add to draft" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("too long to combine");
  expect(native.claim).not.toHaveBeenCalled();
});

it("keeps Add disabled after a reload reports a claimed reply", async () => {
  const native = bridge();
  native.load.mockResolvedValue({ sessionGeneration: "generation-2", items: [{ ...recovery, state: "claimed" }] });
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-2" unlocked draftText="" onAppend={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "Unsent replies (1)" }));
  expect(screen.getByRole("button", { name: "Add to draft" })).toBeDisabled();
  expect(native.claim).not.toHaveBeenCalled();
});

it("ignores a deferred result after its session context changes", async () => {
  let resolveOld!: (value: { sessionGeneration: string; items: (typeof recovery)[] }) => void;
  const native = bridge();
  native.load.mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve; })).mockResolvedValue({ sessionGeneration: "generation-2", items: [] });
  const { rerender } = render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="old" unlocked draftText="" onAppend={vi.fn()} />);
  rerender(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="new" unlocked draftText="" onAppend={vi.fn()} />);
  resolveOld({ sessionGeneration: "generation-1", items: [recovery] });
  await waitFor(() => expect(screen.queryByRole("button", { name: /Unsent notification replies/ })).not.toBeInTheDocument());
});

it("claims once when rapid clicks race a deferred claim", async () => {
  let resolveClaim!: (value: boolean) => void;
  const native = bridge();
  native.claim.mockImplementationOnce(() => new Promise(resolve => { resolveClaim = resolve; }));
  const append = vi.fn().mockResolvedValue(undefined);
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-1" unlocked draftText="" onAppend={append} />);
  fireEvent.click(await screen.findByRole("button", { name: "Unsent replies (1)" }));
  const add = screen.getByRole("button", { name: "Add to draft" });
  fireEvent.click(add); fireEvent.click(add);
  expect(native.claim).toHaveBeenCalledTimes(1);
  resolveClaim(true);
  await waitFor(() => expect(append).toHaveBeenCalledTimes(1));
});

it("shows a one-line opener with the unsent count and title", async () => {
  const native = bridge();
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-1" unlocked draftText="" onAppend={vi.fn()} />);
  const opener = await screen.findByRole("button", { name: "Unsent replies (1)" });
  expect(opener).toHaveAttribute("title", peppyCopy.composer_status_unsent_replies_title);
  expect(opener.closest("#notification-reply-recovery-status")).toBeInTheDocument();
});

it("shows the checking label while every reply is uncertain", async () => {
  const native = bridge();
  native.load.mockResolvedValue({ sessionGeneration: "generation-1", items: [{ ...recovery, state: "uncertain" }] });
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-1" unlocked draftText="" onAppend={vi.fn()} />);
  expect(await screen.findByRole("button", { name: "Checking replies…" })).toBeInTheDocument();
});

it("shows a one-line load error with a retry that reloads", async () => {
  const native = bridge();
  native.load.mockRejectedValueOnce(new Error("offline")).mockRejectedValueOnce(new Error("offline")).mockResolvedValue({ sessionGeneration: "generation-1", items: [recovery] });
  render(<NotificationReplyRecovery bridge={native} conversationId="conversation-1" sessionKey="session-1" unlocked draftText="" onAppend={vi.fn()} />);
  expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.composer_status_unsent_replies_error);
  fireEvent.click(screen.getByRole("button", { name: peppyCopy.composer_status_unsent_replies_retry }));
  expect(await screen.findByRole("button", { name: "Unsent replies (1)" })).toBeInTheDocument();
  expect(native.load).toHaveBeenCalledTimes(3);
});
