import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { PRIVACY_POLICY_URL, PrivacySettings } from "./PrivacySettings";

afterEach(() => {
  cleanup();
});

describe("PrivacySettings", () => {
  it("uses a safe browser link without loading the native opener", () => {
    render(<PrivacySettings browserHost bridge={{ open_external_url: vi.fn() }} />);

    const link = screen.getByRole("link", { name: /privacy policy/i });
    expect(link).toHaveAttribute("href", PRIVACY_POLICY_URL);
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
  });

  it("keeps the scope note as the link's accessible description and marks the footer section", () => {
    const { container } = render(<PrivacySettings browserHost={false} bridge={{ open_external_url: vi.fn() }} />);

    const button = screen.getByRole("button", { name: "Privacy Policy" });
    expect(button).toHaveAccessibleDescription(peppyCopy.privacy_policy_scope);
    expect(container.querySelector("footer#settings-privacy")).toHaveAttribute("data-settings-section", "privacy-links");
    expect(screen.queryByText(peppyCopy.privacy_policy_scope, { selector: ":not(.visually-hidden)" })).not.toBeInTheDocument();
  });

  it("opens only the canonical policy URL from the native host bridge", async () => {
    const openExternalUrl = vi.fn();
    render(<PrivacySettings browserHost={false} bridge={{ open_external_url: openExternalUrl }} />);

    fireEvent.click(screen.getByRole("button", { name: "Privacy Policy" }));

    await vi.waitFor(() => expect(openExternalUrl).toHaveBeenCalledWith(PRIVACY_POLICY_URL));
  });

  it("shows a fixed error when the Privacy Policy bridge opener fails", async () => {
    const openExternalUrl = vi.fn().mockRejectedValueOnce(new Error("sensitive native failure"));
    render(<PrivacySettings browserHost={false} bridge={{ open_external_url: openExternalUrl }} />);

    fireEvent.click(screen.getByRole("button", { name: "Privacy Policy" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Privacy Policy could not be opened.");
    expect(screen.queryByText("sensitive native failure")).not.toBeInTheDocument();
  });

  it("shows the fixed Privacy Policy error when the bridge has no opener", async () => {
    render(<PrivacySettings browserHost={false} bridge={{}} />);

    fireEvent.click(screen.getByRole("button", { name: "Privacy Policy" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Privacy Policy could not be opened.");
  });

  it("uses a safe browser link for Guides", () => {
    render(<PrivacySettings browserHost bridge={{ open_external_url: vi.fn() }} />);

    const link = screen.getByRole("link", { name: /guides/i });
    expect(link).toHaveAttribute("href", "https://peppy.pro/docs");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    expect(link).not.toHaveAccessibleDescription(peppyCopy.privacy_policy_scope);
  });

  it("opens Guides through the bridge and clears a fixed failure on retry", async () => {
    const openExternalUrl = vi.fn().mockRejectedValueOnce(new Error("app-locked")).mockResolvedValue(undefined);
    render(<PrivacySettings browserHost={false} bridge={{ open_external_url: openExternalUrl }} />);

    fireEvent.click(screen.getByRole("button", { name: "Guides" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Guides could not be opened.");
    expect(screen.queryByText("app-locked")).not.toBeInTheDocument();
    expect(screen.queryByText("https://peppy.pro/docs")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Guides" }));

    await vi.waitFor(() => expect(openExternalUrl).toHaveBeenLastCalledWith("https://peppy.pro/docs"));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows the fixed Guides error when the bridge has no opener", async () => {
    render(<PrivacySettings browserHost={false} bridge={{}} />);

    fireEvent.click(screen.getByRole("button", { name: "Guides" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Guides could not be opened.");
  });
});
