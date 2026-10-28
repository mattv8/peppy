import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TonePicker } from "./TonePicker";

describe("TonePicker", () => {
  afterEach(cleanup);
  it("moves focus without persisting until the active color is confirmed", () => {
    const onSelect = vi.fn();
    render(<TonePicker id="tone" seed="contact" automaticIndex={9} value={60} onSelect={onSelect} />);
    fireEvent.click(screen.getByRole("button", { name: "Change conversation color" }));
    const red = screen.getByRole("radio", { name: "Red" });
    fireEvent.keyDown(red, { key: "ArrowRight" });
    expect(onSelect).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByRole("radio", { name: "Orange" }), { key: "Enter" });
    expect(onSelect).toHaveBeenCalledWith(15);
  });

  it("resets to automatic and keeps the panel open when saving fails", () => {
    const onSelect = vi.fn();
    const { rerender } = render(<TonePicker id="tone" seed="contact" automaticIndex={9} value={60} error="Couldn't save the color. Try again." onSelect={onSelect} />);
    fireEvent.click(screen.getByRole("button", { name: "Change conversation color" }));
    fireEvent.click(screen.getByRole("radio", { name: "Automatic" }));
    expect(onSelect).toHaveBeenCalledWith(null);
    rerender(<TonePicker id="tone" seed="contact" automaticIndex={9} value={60} error="Couldn't save the color. Try again." onSelect={onSelect} />);
    expect(screen.getByRole("alert")).toBeVisible();
  });
});
