import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ContactAvatar } from "./ContactAvatar";
import { paletteIndex } from "./avatar";

afterEach(cleanup);

describe("ContactAvatar", () => {
  it("selects photo, initials, person, and group glyphs by priority", () => {
    const { rerender } = render(<ContactAvatar seed="photo" photoDataUrl="data:image/png;base64,photo" name="Ada Lovelace" />);
    expect(document.querySelector(".contact-avatar")).toHaveAttribute("data-avatar-kind", "photo");

    rerender(<ContactAvatar seed="initials" name="Ada Lovelace" />);
    expect(document.querySelector(".contact-avatar")).toHaveAttribute("data-avatar-kind", "initials");
    expect(document.querySelector(".contact-avatar")).toHaveTextContent("AL");

    rerender(<ContactAvatar seed="small" name="Ada Lovelace" size={18} />);
    expect(document.querySelector(".contact-avatar")).toHaveTextContent("A");

    rerender(<ContactAvatar seed="person" />);
    expect(document.querySelector(".contact-avatar")).toHaveAttribute("data-avatar-kind", "person");

    rerender(<ContactAvatar seed="group" kind="group" photoDataUrl="data:image/png;base64,group" name="Ada Lovelace" />);
    expect(document.querySelector(".contact-avatar")).toHaveAttribute("data-avatar-kind", "group");
  });

  it("falls back after a failed photo and retries when its URL changes", () => {
    const { rerender } = render(<ContactAvatar seed="ada" photoDataUrl="data:image/png;base64,broken" name="Ada Lovelace" />);
    fireEvent.error(document.querySelector(".contact-avatar img")!);
    expect(document.querySelector(".contact-avatar")).toHaveAttribute("data-avatar-kind", "initials");

    rerender(<ContactAvatar seed="ada" photoDataUrl="data:image/png;base64,broken" name="Ada Lovelace" />);
    expect(document.querySelector(".contact-avatar")).toHaveAttribute("data-avatar-kind", "initials");

    rerender(<ContactAvatar seed="ada" photoDataUrl="data:image/png;base64,replacement" name="Ada Lovelace" />);
    expect(document.querySelector(".contact-avatar")).toHaveAttribute("data-avatar-kind", "photo");
  });

  it("uses the deterministic palette color variable", () => {
    render(<ContactAvatar seed="+18015551234" />);
    const avatar = document.querySelector(".contact-avatar")!;
    const index = paletteIndex("+18015551234");
    expect(avatar).toHaveAttribute("data-avatar-color", String(index));
    expect(avatar).toHaveStyle(`--peppy-tone: var(--peppy-avatar-${index}, var(--peppy-accent))`);
  });
});
