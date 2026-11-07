import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
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

  it("moves six grid positions with ArrowDown and omits the help text", () => {
    const onSelect = vi.fn();
    render(<TonePicker id="tone" seed="contact" automaticIndex={9} value={60} onSelect={onSelect} />);
    fireEvent.click(screen.getByRole("button", { name: "Change conversation color" }));
    fireEvent.keyDown(screen.getByRole("radio", { name: "Red" }), { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("radio", { name: "Green" }));
    expect(screen.queryByText(/on all your devices/)).toBeNull();
  });

  it("keeps the panel open without an announcement after an error or unrelated value change", () => {
    let failRequest = () => {};
    let changeValue = () => {};
    let changeSeed = () => {};
    function ErrorPicker() {
      const [seed, setSeed] = useState("contact");
      const [value, setValue] = useState(60);
      const [pending, setPending] = useState(false);
      const [error, setError] = useState<string>();
      failRequest = () => { setPending(false); setError("Couldn't save the color. Try again."); };
      changeValue = () => { setError(undefined); setValue(7); };
      changeSeed = () => { setSeed("another-contact"); setValue(15); };
      return <TonePicker id="tone" seed={seed} automaticIndex={9} value={value} pending={pending} error={error} onSelect={() => setPending(true)} />;
    }

    render(<ErrorPicker />);
    fireEvent.click(screen.getByRole("button", { name: "Change conversation color" }));
    fireEvent.click(screen.getByRole("radio", { name: "Orange" }));
    expect(screen.getByRole("status")).toHaveTextContent("Saving…");
    act(failRequest);
    expect(screen.getByRole("alert")).toBeVisible();
    expect(screen.getByRole("dialog", { name: "Conversation color" })).toBeVisible();
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
    act(changeValue);
    expect(screen.getByRole("dialog", { name: "Conversation color" })).toBeVisible();
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
    act(changeSeed);
    expect(screen.queryByRole("dialog", { name: "Conversation color" })).toBeNull();
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
  });

  it("discards a pending request when its seed changes", async () => {
    let changeSeed = () => {};
    let finishRequest = () => {};
    function SeedChangingPicker() {
      const [seed, setSeed] = useState("contact");
      const [value, setValue] = useState(60);
      const [pending, setPending] = useState(false);
      changeSeed = () => setSeed("another-contact");
      finishRequest = () => { setValue(15); setPending(false); };
      return <TonePicker id="tone" seed={seed} automaticIndex={9} value={value} pending={pending} onSelect={() => setPending(true)} />;
    }

    render(<SeedChangingPicker />);
    const trigger = screen.getByRole("button", { name: "Change conversation color" });
    fireEvent.click(trigger);
    const orange = screen.getByRole("radio", { name: "Orange" });
    orange.focus();
    fireEvent.click(orange);
    expect(screen.getByRole("status")).toHaveTextContent("Saving…");
    act(changeSeed);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Conversation color" })).toBeNull());
    expect(trigger).not.toHaveFocus();
    act(finishRequest);
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
    expect(trigger).not.toHaveFocus();
  });

  it("does not complete an ignored request after the picker is reopened", async () => {
    let matchRequestedValue = () => {};
    function IgnoredRequestPicker() {
      const [value, setValue] = useState(60);
      matchRequestedValue = () => setValue(15);
      return <TonePicker id="tone" seed="contact" automaticIndex={9} value={value} onSelect={() => {}} />;
    }

    render(<IgnoredRequestPicker />);
    const trigger = screen.getByRole("button", { name: "Change conversation color" });
    fireEvent.click(trigger);
    const orange = screen.getByRole("radio", { name: "Orange" });
    fireEvent.click(orange);
    expect(screen.getByRole("dialog", { name: "Conversation color" })).toBeVisible();
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
    fireEvent.keyDown(orange, { key: "Escape" });
    await waitFor(() => expect(trigger).toHaveFocus());
    fireEvent.click(trigger);
    await waitFor(() => expect(screen.getByRole("radio", { name: "Red" })).toHaveFocus());
    act(matchRequestedValue);
    expect(screen.getByRole("dialog", { name: "Conversation color" })).toBeVisible();
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
    expect(trigger).not.toHaveFocus();
  });

  it("supports custom labels and swatches", () => {
    const swatchColor = vi.fn((index: number | null) => `var(--accent-${index ?? "default"})`);
    render(<TonePicker id="tone" seed="contact" automaticIndex={9} value={60} label="Accent color" defaultLabel="Default (Coral)" swatchColor={swatchColor} variant="field" onSelect={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Accent color: Red" })).toHaveAttribute("aria-label", "Accent color: Red");
    expect(document.querySelector("[data-tone-picker-swatch]")).toHaveStyle({ background: "var(--accent-60)" });
    fireEvent.click(screen.getByRole("button", { name: "Accent color: Red" }));
    expect(screen.getByRole("dialog", { name: "Accent color" })).toBeVisible();
    expect(screen.getByRole("radiogroup", { name: "Accent color" })).toBeVisible();
    expect(screen.getByText("Accent color")).toBeVisible();
    expect(screen.getByRole("radio", { name: "Default (Coral)" })).toHaveStyle("--tone-swatch: var(--accent-default)");
    expect(screen.getByRole("radio", { name: "Red" })).toHaveStyle("--tone-swatch: var(--accent-60)");
  });

  it("completes synchronous selections", async () => {
    function SynchronousPicker() {
      const [value, setValue] = useState<number | undefined>(60);
      return <TonePicker id="tone" seed="contact" automaticIndex={9} value={value} variant="field" label="Accent color" defaultLabel="Default (Coral)" onSelect={(index) => setValue(index ?? undefined)} />;
    }

    render(<SynchronousPicker />);
    const trigger = screen.getByRole("button", { name: "Accent color: Red" });
    fireEvent.click(trigger);
    const defaultOption = screen.getByRole("radio", { name: "Default (Coral)" });
    defaultOption.focus();
    fireEvent.click(defaultOption);
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(screen.queryByRole("dialog", { name: "Accent color" })).toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent("Accent color set to Default (Coral)");
  });

  it("closes after asynchronous selections complete", async () => {
    let finishRequest = () => {};
    function AsynchronousPicker() {
      const [value, setValue] = useState(60);
      const [pending, setPending] = useState(false);
      finishRequest = () => { setValue(15); setPending(false); };
      return <TonePicker id="tone" seed="contact" automaticIndex={9} value={value} pending={pending} onSelect={() => setPending(true)} />;
    }

    render(<AsynchronousPicker />);
    fireEvent.click(screen.getByRole("button", { name: "Change conversation color" }));
    const orange = screen.getByRole("radio", { name: "Orange" });
    orange.focus();
    fireEvent.click(orange);
    expect(screen.getByRole("dialog", { name: "Conversation color" })).toBeVisible();
    expect(screen.getByRole("status")).toHaveTextContent("Saving…");
    act(finishRequest);
    await waitFor(() => expect(screen.getByRole("button", { name: "Change conversation color" })).toHaveFocus());
    expect(screen.queryByRole("dialog", { name: "Conversation color" })).toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent("Conversation color set to Orange");
  });
});
