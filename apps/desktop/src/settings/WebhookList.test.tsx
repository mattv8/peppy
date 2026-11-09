import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WebhookList, type WebhookListProps } from "./WebhookList";
import { homeAssistant, n8n, zapier } from "./webhookTestFixtures";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((complete, fail) => {
    resolve = complete;
    reject = fail;
  });
  return { promise, resolve, reject };
}

function renderList(props: Partial<WebhookListProps> = {}) {
  const handlers = {
    onEdit: vi.fn(),
    onSetPaused: vi.fn().mockResolvedValue(undefined),
    onDelete: vi.fn().mockResolvedValue(undefined),
  };
  const result = render(
    <WebhookList
      webhooks={[homeAssistant, zapier, n8n]}
      serverOffline={false}
      {...handlers}
      {...props}
    />,
  );
  return { ...result, ...handlers };
}

const row = (id: string) => document.getElementById(`webhook-${id}`) as HTMLElement;

describe("WebhookList", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders one row per webhook with its URL, filter summary and last delivery", () => {
    renderList();

    expect(screen.getByRole("list", { name: "Webhooks" })).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
    expect(row(homeAssistant.id)).toHaveAttribute("data-webhook", homeAssistant.id);
    expect(row(homeAssistant.id)).toHaveTextContent("https://ha.example.com/api/webhook/peppy-in");
    expect(row(homeAssistant.id)).toHaveTextContent("All messages");
    expect(row(homeAssistant.id)).toHaveTextContent("200 · 3m");
    expect(row(zapier.id)).toHaveTextContent("Only Alice Chen, Bob Ruiz");
    expect(row(zapier.id)).toHaveTextContent("Failing · 503");
    expect(row(n8n.id)).toHaveTextContent("Everyone except Mom, Dad, +1");
    expect(row(n8n.id)).toHaveTextContent("—");
  });

  it("names each switch after its webhook and carries the state in the checked value", () => {
    renderList();

    expect(within(row(n8n.id)).getByText("Paused")).toBeInTheDocument();
    expect(row(n8n.id)).toHaveAttribute("data-paused", "true");
    expect(screen.getByRole("switch", { name: "n8n triage active" })).not.toBeChecked();
    expect(screen.getByRole("switch", { name: "Home Assistant active" })).toBeChecked();
    expect(screen.queryByRole("switch", { name: /^(Pause|Resume) / })).not.toBeInTheDocument();
  });

  it("exposes the last delivery to assistive tech as a status image on the name line", () => {
    renderList();

    expect(within(row(zapier.id)).getByRole("img", { name: "Last delivery: failing, 503" })).toBeInTheDocument();
    expect(within(row(homeAssistant.id)).getByRole("img", { name: "Last delivery: delivered, 200" })).toBeInTheDocument();
  });

  it("opens the editor from the Edit button, passing the trigger for focus return", () => {
    const { onEdit } = renderList();

    fireEvent.click(screen.getByRole("button", { name: "Edit Zapier: Alice & Bob" }));

    expect(onEdit).toHaveBeenCalledWith(zapier, expect.any(HTMLElement));
  });

  it("pauses a running webhook through onSetPaused with the flipped state", () => {
    const { onSetPaused } = renderList();

    fireEvent.click(screen.getByRole("switch", { name: "Home Assistant active" }));

    expect(onSetPaused).toHaveBeenCalledWith(homeAssistant, true);
  });

  it("resumes a paused webhook and keeps its switch focusable while the change is pending", async () => {
    const pending = deferred<void>();
    const onSetPaused = vi.fn().mockReturnValue(pending.promise);
    renderList({ onSetPaused });

    const toggle = screen.getByRole("switch", { name: "n8n triage active" });
    fireEvent.click(toggle);

    expect(onSetPaused).toHaveBeenCalledWith(n8n, false);
    expect(toggle).toBeEnabled();
    expect(toggle).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByRole("button", { name: "Edit n8n triage" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Delete n8n triage" })).toBeDisabled();
    expect(row(n8n.id)).toHaveAttribute("aria-busy", "true");

    pending.resolve();
    await waitFor(() => expect(toggle).not.toHaveAttribute("aria-disabled"));
  });

  it("shows a row-level error when the pause change fails", async () => {
    renderList({ onSetPaused: vi.fn().mockRejectedValue(new Error("offline")) });

    fireEvent.click(screen.getByRole("switch", { name: "Home Assistant active" }));

    expect(await within(row(homeAssistant.id)).findByRole("alert")).toHaveTextContent("Couldn't pause Home Assistant. Try again.");
  });

  it("disables pause and delete while the server is offline", () => {
    renderList({ serverOffline: true });

    expect(screen.getByRole("switch", { name: "Home Assistant active" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Delete Home Assistant" })).toBeDisabled();
  });

  it("asks for inline confirmation, focusing Cancel first", () => {
    const { onDelete } = renderList();

    fireEvent.click(screen.getByRole("button", { name: "Delete Zapier: Alice & Bob" }));

    const confirm = row(zapier.id);
    expect(confirm).toHaveAttribute("data-webhook-confirm");
    expect(within(confirm).getByText(/queued deliveries are dropped/i)).toBeInTheDocument();
    expect(within(confirm).getByRole("button", { name: "Cancel" })).toHaveFocus();
    expect(onDelete).not.toHaveBeenCalled();
  });

  it("restores focus to the row's Delete button when the confirmation is cancelled", () => {
    const { onDelete } = renderList();

    fireEvent.click(screen.getByRole("button", { name: "Delete Zapier: Alice & Bob" }));
    fireEvent.click(within(row(zapier.id)).getByRole("button", { name: "Cancel" }));

    expect(row(zapier.id)).not.toHaveAttribute("data-webhook-confirm");
    expect(screen.getByRole("button", { name: "Delete Zapier: Alice & Bob" })).toHaveFocus();
    expect(onDelete).not.toHaveBeenCalled();
  });

  it("deletes only after the explicit confirm button", async () => {
    const { onDelete } = renderList();

    fireEvent.click(screen.getByRole("button", { name: "Delete Zapier: Alice & Bob" }));
    fireEvent.click(within(row(zapier.id)).getByRole("button", { name: "Delete" }));

    await waitFor(() => expect(onDelete).toHaveBeenCalledWith(zapier));
  });

  it("keeps the confirmation open with the error and refocuses Delete when deletion fails", async () => {
    const onDelete = vi.fn().mockRejectedValue({ code: "webhook_not_found", message: "gone" });
    renderList({ onDelete });

    fireEvent.click(screen.getByRole("button", { name: "Delete Zapier: Alice & Bob" }));
    fireEvent.click(within(row(zapier.id)).getByRole("button", { name: "Delete" }));

    const alert = await within(row(zapier.id)).findByRole("alert");
    expect(alert).toHaveTextContent("This webhook no longer exists.");
    await waitFor(() => expect(within(row(zapier.id)).getByRole("button", { name: "Delete" })).toHaveFocus());
  });

  it("refocuses Delete after a delayed deletion failure", async () => {
    const pending = deferred<void>();
    renderList({ onDelete: vi.fn().mockReturnValue(pending.promise) });

    fireEvent.click(screen.getByRole("button", { name: "Delete Zapier: Alice & Bob" }));
    fireEvent.click(within(row(zapier.id)).getByRole("button", { name: "Delete" }));
    pending.reject({ code: "webhook_not_found", message: "gone" });

    await within(row(zapier.id)).findByRole("alert");
    await waitFor(() => expect(within(row(zapier.id)).getByRole("button", { name: "Delete" })).toHaveFocus());
  });
});
