import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PRIVACY_POLICY_URL, PrivacySettings } from "./PrivacySettings";

const openUrl = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl }));

afterEach(() => {
  cleanup();
  openUrl.mockReset();
});

describe("PrivacySettings", () => {
  it("uses a safe browser link without loading the native opener", () => {
    render(<PrivacySettings browserHost />);

    const link = screen.getByRole("link", { name: /privacy policy/i });
    expect(link).toHaveAttribute("href", PRIVACY_POLICY_URL);
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    expect(openUrl).not.toHaveBeenCalled();
  });

  it("opens only the canonical policy URL from the native host", async () => {
    render(<PrivacySettings browserHost={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Privacy Policy" }));

    await vi.waitFor(() => expect(openUrl).toHaveBeenCalledWith(PRIVACY_POLICY_URL));
  });

  it("shows a fixed error when the native opener fails", async () => {
    openUrl.mockRejectedValueOnce(new Error("sensitive native failure"));
    render(<PrivacySettings browserHost={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Privacy Policy" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Privacy Policy could not be opened.");
    expect(screen.queryByText("sensitive native failure")).not.toBeInTheDocument();
  });
});
