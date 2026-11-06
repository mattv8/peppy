import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AppLockScreen, AppLockSettings } from "./AppLock";

describe("AppLockScreen", () => {
  it("shows a disabled unlock action while checking secure storage", () => {
    const onUnlock = vi.fn();
    render(<AppLockScreen state="checking" onUnlock={onUnlock} onRetry={async () => undefined} />);

    expect(screen.getByRole("main")).toHaveAttribute("data-app-lock-state", "checking");
    expect(screen.getByRole("main")).toHaveAttribute("aria-busy", "true");
    expect(screen.getByRole("heading", { name: "Checking Peppy's lock" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Peppy is checking its password setting in secure storage.");
    const unlock = screen.getByRole("button", { name: "Unlock" });
    expect(unlock).toBeDisabled();
    fireEvent.click(unlock);
    expect(onUnlock).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Try again" })).not.toBeInTheDocument();
  });

  it("disables retry while it is pending and reports a retry-specific failure", async () => {
    const onRetry = vi.fn().mockRejectedValue({});
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    render(<AppLockScreen state="storage-unavailable" onUnlock={async () => false} onRetry={onRetry} />);

    const retry = screen.getByRole("button", { name: "Try again" });
    fireEvent.click(retry);
    expect(onRetry).toHaveBeenCalledOnce();
    expect(retry).toBeDisabled();
    expect(retry).toHaveAttribute("aria-busy", "true");
    expect(await screen.findByRole("alert")).toHaveTextContent("Peppy still cannot reach secure storage.");
    expect(screen.getByRole("alert")).not.toHaveTextContent("password requirement");
    await waitFor(() => expect(retry).toHaveFocus());
  });
});

describe("AppLockSettings", () => {
  it("disables the password setting while app lock status is pending", () => {
    render(<AppLockSettings status={{ enabled: false, locked: true, storageAvailable: true, pending: true }} onChange={async () => true} />);

    expect(screen.getByRole("checkbox", { name: "Require a password when Peppy starts" })).toBeDisabled();
  });
});
