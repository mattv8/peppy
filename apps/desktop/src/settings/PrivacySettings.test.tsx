import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
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

  it("keeps the scope note as the link's accessible description and marks the footer section", () => {
    const { container } = render(<PrivacySettings browserHost={false} />);

    const button = screen.getByRole("button", { name: "Privacy Policy" });
    expect(button).toHaveAccessibleDescription(peppyCopy.privacy_policy_scope);
    expect(container.querySelector("footer#settings-privacy")).toHaveAttribute("data-settings-section", "privacy-links");
    expect(screen.queryByText(peppyCopy.privacy_policy_scope, { selector: ":not(.visually-hidden)" })).not.toBeInTheDocument();
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

  it("shows the typing sharing preference only when it is available", () => {
    const onChange = vi.fn();
    const { rerender } = render(<PrivacySettings browserHost={false} />);

    expect(document.querySelector("#settings-share-typing")).toBeNull();

    rerender(<PrivacySettings browserHost={false} typingShare={{ enabled: true, onChange }} />);
    const control = document.querySelector<HTMLInputElement>("#settings-share-typing")!;
    expect(control).toBeChecked();
    expect(screen.queryByText(peppyCopy.typing_share_hint)).not.toBeInTheDocument();
    const info = screen.getByRole("button", { name: peppyCopy.typing_share_info_label });
    fireEvent.click(info);
    expect(screen.getByRole("dialog", { name: peppyCopy.typing_share_info_label })).toHaveTextContent(peppyCopy.typing_share_hint);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(info).toHaveFocus();
    fireEvent.click(control);
    expect(onChange).toHaveBeenCalledWith(false);
  });

  it("shows the generic setting error when sharing cannot be saved", () => {
    render(<PrivacySettings browserHost={false} typingShare={{ enabled: true, onChange: vi.fn(), error: true }} />);

    expect(screen.getByRole("alert")).toHaveAttribute("id", "settings-share-typing-error");
    expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.toggle_android_phone_read_error);
  });
});
