import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { RcsFeaturesSettings } from "./RcsFeaturesSettings";

afterEach(() => {
  cleanup();
});

describe("RcsFeaturesSettings", () => {
  it("shows the typing sharing preference in the RCS Features section", () => {
    const onChange = vi.fn();
    const { container } = render(<RcsFeaturesSettings typingShare={{ enabled: true, onChange }} />);

    expect(screen.getByRole("heading", { name: "RCS Features" })).toBeInTheDocument();
    expect(container.querySelector("section#settings-rcs-features")).toHaveAttribute("data-settings-section", "rcs-features");

    const control = document.querySelector<HTMLInputElement>("#settings-share-typing")!;
    expect(control).toBeChecked();
    expect(document.querySelector("#settings-share-typing-info")).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    fireEvent.click(control);
    expect(onChange).toHaveBeenCalledWith(false);
  });

  it("shows the generic setting error when sharing cannot be saved", () => {
    render(<RcsFeaturesSettings typingShare={{ enabled: true, onChange: vi.fn(), error: true }} />);

    expect(screen.getByRole("alert")).toHaveAttribute("id", "settings-share-typing-error");
    expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.setting_update_error);
  });
});
