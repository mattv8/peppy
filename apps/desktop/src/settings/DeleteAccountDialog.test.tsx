import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createRef, useRef, useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { bridge } from "../bridge/bridge";
import { DeleteAccountDialog } from "./DeleteAccountDialog";

vi.mock("../bridge/bridge", { spy: true });

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(bridge.billing_summary).mockResolvedValue({ provider: "apple", manageableIn: "app_store", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false });
});
afterEach(cleanup);

it("keeps deletion confirmation intact when subscription management fails and clears the error on retry", async () => {
  let finishRetry!: () => void;
  const retry = new Promise<void>(resolve => { finishRetry = resolve; });
  vi.mocked(bridge.open_store_subscriptions).mockRejectedValueOnce(new Error("unavailable")).mockReturnValueOnce(retry);
  const onDeleted = vi.fn();
  const onDismiss = vi.fn();
  render(<DeleteAccountDialog billing={{ provider: "apple", manageableIn: "app_store", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }} opener={createRef()} onDismiss={onDismiss} onDeleted={onDeleted} />);
  const confirmation = screen.getByRole("checkbox", { name: "I understand this is permanent" });
  const dialog = screen.getByRole("dialog");
  const manage = screen.getByRole("button", { name: "Manage App Store subscriptions" });
  fireEvent.click(confirmation);
  manage.focus();
  fireEvent.click(manage);

  await waitFor(() => expect(document.getElementById("account-delete-management-error")).toHaveTextContent("App Store subscriptions couldn't be opened. Try again."));
  expect(confirmation).toBeChecked();
  expect(dialog).toBeVisible();
  expect(manage).toHaveFocus();
  expect(bridge.hosted_delete_account).not.toHaveBeenCalled();
  expect(onDismiss).not.toHaveBeenCalled();
  expect(onDeleted).not.toHaveBeenCalled();

  fireEvent.click(confirmation);
  fireEvent.click(confirmation);
  expect(document.getElementById("account-delete-management-error")).toBeVisible();
  expect(bridge.open_store_subscriptions).toHaveBeenCalledTimes(1);

  fireEvent.click(manage);
  await waitFor(() => expect(bridge.open_store_subscriptions).toHaveBeenCalledTimes(2));
  expect(document.getElementById("account-delete-management-error")).not.toBeInTheDocument();
  await act(async () => finishRetry());
  expect(document.getElementById("account-delete-management-error")).not.toBeInTheDocument();
  expect(confirmation).toBeChecked();
  expect(dialog).toBeVisible();
  expect(bridge.hosted_delete_account).not.toHaveBeenCalled();
  expect(onDismiss).not.toHaveBeenCalled();
});

it("restores focus to the opener after ordinary cancellation", async () => {
  function DeletionSettings() {
    const opener = useRef<HTMLButtonElement>(null);
    const [open, setOpen] = useState(false);
    return <><button ref={opener} onClick={() => setOpen(true)}>Open deletion</button>{open && <DeleteAccountDialog billing={{ provider: "apple", manageableIn: "app_store", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }} opener={opener} onDismiss={() => setOpen(false)} onDeleted={vi.fn()} />}</>;
  }
  render(<DeletionSettings />);
  const opener = screen.getByRole("button", { name: "Open deletion" });
  fireEvent.click(opener);
  expect(screen.getByRole("dialog")).toBeVisible();

  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  await waitFor(() => expect(opener).toHaveFocus());
  expect(bridge.hosted_delete_account).not.toHaveBeenCalled();
});
