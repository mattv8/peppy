import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { GatewayPicker, type GatewayPickerOption } from "./GatewayPicker";

const options: GatewayPickerOption[] = [
  { value: "pixel:sim-1", name: "Pixel 8", simLabel: "SIM 1", statusLabel: "Connected", availability: "connected", gatewayId: "pixel", simId: "sim-1" },
  { value: "iphone:sim-2", name: "iPhone", simLabel: "SIM 2", statusLabel: "SMS unavailable", availability: "unsupported_platform", disabled: true, gatewayId: "iphone", simId: "sim-2" },
  { value: "galaxy:sim-3", name: "Galaxy S24", simLabel: "SIM 3", statusLabel: "Delayed", availability: "delayed", simulated: true, gatewayId: "galaxy", simId: "sim-3" },
];

function renderPicker(overrides: Partial<ComponentProps<typeof GatewayPicker>> = {}) {
  const onChange = vi.fn();
  const { unmount } = render(
    <div className="theme-dark">
      <GatewayPicker options={options} value="pixel:sim-1" placeholder="Choose phone" onChange={onChange} {...overrides} />
      <button type="button">Outside</button>
    </div>,
  );
  return { onChange, trigger: screen.getByRole("combobox", { name: "Phone" }), unmount };
}

describe("GatewayPicker", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("commits the exact route value selected by pointer", () => {
    const { onChange, trigger } = renderPicker();

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("option", { name: /Galaxy S24/ }));

    expect(onChange).toHaveBeenCalledWith("galaxy:sim-3");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("skips disabled options while navigating and commits the active option", () => {
    const { onChange, trigger } = renderPicker();

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("galaxy"));
    fireEvent.keyDown(trigger, { key: "ArrowUp" });
    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("pixel"));
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    fireEvent.keyDown(trigger, { key: "Enter" });

    expect(onChange).toHaveBeenCalledWith("galaxy:sim-3");
  });

  it("commits the first enabled route with Home and Space", () => {
    const { onChange, trigger } = renderPicker({ value: "galaxy:sim-3" });

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "Home" });
    fireEvent.keyDown(trigger, { key: " " });

    expect(onChange).toHaveBeenCalledWith("pixel:sim-1");
  });

  it("cancels an open selection with Escape and closes on Tab", () => {
    const { onChange, trigger } = renderPicker();

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(onChange).not.toHaveBeenCalled();

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "Tab" });
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("typeahead selects an enabled matching route", () => {
    const { onChange, trigger } = renderPicker();

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "g" });
    fireEvent.keyDown(trigger, { key: "Enter" });

    expect(onChange).toHaveBeenCalledWith("galaxy:sim-3");
  });

  it("focuses the trigger when a pointer opens the picker", () => {
    const { trigger } = renderPicker();
    const outside = screen.getByRole("button", { name: "Outside" });
    outside.focus();

    fireEvent.click(trigger);

    expect(document.activeElement).toBe(trigger);
  });

  it.each(["ArrowDown", "ArrowUp"])("opens closed %s on the selected enabled route", (key) => {
    const { trigger } = renderPicker({ value: "galaxy:sim-3" });

    fireEvent.keyDown(trigger, { key });

    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("galaxy"));
  });

  it("closes without committing when the selected route is chosen again", () => {
    const { onChange, trigger } = renderPicker();

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("option", { name: /Pixel 8/ }));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("prevents popup background mousedown defaults while retaining trigger focus", () => {
    const { trigger } = renderPicker();
    trigger.focus();
    fireEvent.click(trigger);
    const popup = screen.getByRole("listbox");

    expect(fireEvent.mouseDown(popup)).toBe(false);
    expect(document.activeElement).toBe(trigger);
  });

  it("extends pending typeahead with Space without committing", () => {
    vi.useFakeTimers();
    const typeaheadOptions = [
      options[0],
      { ...options[2], value: "smart:sim-3", name: "S Phone", simLabel: undefined, statusLabel: undefined },
    ];
    const { onChange, trigger } = renderPicker({ options: typeaheadOptions });

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "s" });
    fireEvent.keyDown(trigger, { key: " " });

    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("smart"));
    expect(onChange).not.toHaveBeenCalled();
    vi.advanceTimersByTime(500);
    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("smart:sim-3");
  });

  it("keeps typeahead pending until 500ms after the most recent search key", () => {
    vi.useFakeTimers();
    const { trigger } = renderPicker();

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "g" });
    vi.advanceTimersByTime(400);
    fireEvent.keyDown(trigger, { key: "a" });
    vi.advanceTimersByTime(200);
    fireEvent.keyDown(trigger, { key: "p" });

    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("galaxy"));
    vi.advanceTimersByTime(501);
    fireEvent.keyDown(trigger, { key: "p" });
    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("pixel"));
  });

  it("keeps the missing-SIM availability badge when its label matches the SIM label", () => {
    const unavailable = {
      ...options[0],
      simLabel: "No SMS SIM",
      statusLabel: "No SMS SIM",
      availability: "no_sms_sim" as const,
    };
    const { trigger } = renderPicker({ options: [unavailable], value: unavailable.value });

    expect(trigger.querySelector('[data-route-badge="status"][data-gateway-availability="no_sms_sim"]')).toBeInTheDocument();
    expect(trigger.querySelector('[data-route-badge="sim"]')).not.toBeInTheDocument();
  });

  it("keeps the active descendant valid when choices change while open", () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <div className="theme-dark">
        <GatewayPicker options={options} value="pixel:sim-1" placeholder="Choose phone" onChange={onChange} />
      </div>,
    );
    const trigger = screen.getByRole("combobox", { name: "Phone" });

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "End" });
    rerender(
      <div className="theme-dark">
        <GatewayPicker options={[options[0]]} value="pixel:sim-1" placeholder="Choose phone" onChange={onChange} />
      </div>,
    );
    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("pixel"));
    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(onChange).not.toHaveBeenCalled();
  });

  it("keeps an all-disabled list visible without committing a route", () => {
    const onChange = vi.fn();
    const disabledOption = { ...options[1], value: "unsupported iPhone SIM 2" };
    render(
      <div className="theme-dark">
        <GatewayPicker options={[disabledOption]} value={disabledOption.value} placeholder="Choose phone" onChange={onChange} />
      </div>,
    );
    const trigger = screen.getByRole("combobox", { name: "Phone" });

    fireEvent.click(trigger);
    expect(screen.getByRole("option", { name: /iPhone/ })).toHaveAttribute("aria-disabled", "true");
    expect(trigger).not.toHaveAttribute("aria-activedescendant");
    fireEvent.click(screen.getByRole("option", { name: /iPhone/ }));
    expect(onChange).not.toHaveBeenCalled();
  });

  it("replaces an active option that becomes disabled while open", () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <div className="theme-dark">
        <GatewayPicker options={options} value="pixel:sim-1" placeholder="Choose phone" onChange={onChange} />
      </div>,
    );
    const trigger = screen.getByRole("combobox", { name: "Phone" });

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "End" });
    rerender(
      <div className="theme-dark">
        <GatewayPicker options={[options[0], { ...options[2], disabled: true }]} value="pixel:sim-1" placeholder="Choose phone" onChange={onChange} />
      </div>,
    );
    expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringContaining("pixel"));
    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(onChange).not.toHaveBeenCalled();
  });

  it("describes the selected route and uses whitespace-safe active option IDs", () => {
    const spacedOption = { ...options[0], value: "gateway id SIM 1" };
    render(
      <div className="theme-dark">
        <span id="route-hint">Route status</span>
        <GatewayPicker options={[spacedOption]} value={spacedOption.value} placeholder="Choose phone" onChange={vi.fn()} describedBy="route-hint" />
      </div>,
    );
    const trigger = screen.getByRole("combobox", { name: "Phone" });

    fireEvent.click(trigger);
    const descriptionIds = trigger.getAttribute("aria-describedby")?.split(" ") ?? [];
    expect(descriptionIds).toContain("route-hint");
    expect(descriptionIds).toHaveLength(2);
    expect(document.getElementById(trigger.getAttribute("aria-activedescendant") ?? "")).toBe(screen.getByRole("option"));
  });

  it("cancels an open list when focus or a pointer moves outside", () => {
    const { onChange, trigger } = renderPicker();
    const outside = screen.getByRole("button", { name: "Outside" });

    fireEvent.click(trigger);
    fireEvent.pointerDown(outside);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    fireEvent.click(trigger);
    outside.focus();
    fireEvent.blur(trigger, { relatedTarget: outside });
    expect(document.activeElement).toBe(outside);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("anchors a popup above a low trigger without pinning short lists to the viewport top", () => {
    const { trigger } = renderPicker();
    vi.stubGlobal("innerHeight", 480);
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue({
      bottom: 430, height: 30, left: 20, right: 220, top: 400, width: 200, x: 20, y: 400, toJSON: () => ({}),
    });

    fireEvent.click(trigger);
    const popup = screen.getByRole("listbox");
    expect(popup).toHaveStyle({ top: "394px", transform: "translateY(-100%)" });
  });

  it("caps below placement and clamps a popup to the viewport gutter", () => {
    const { trigger } = renderPicker();
    vi.stubGlobal("innerHeight", 700);
    vi.stubGlobal("innerWidth", 300);
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue({
      bottom: 100, height: 30, left: -20, right: 180, top: 70, width: 200, x: -20, y: 70, toJSON: () => ({}),
    });

    fireEvent.click(trigger);

    expect(screen.getByRole("listbox")).toHaveStyle({ top: "106px", left: "8px", maxHeight: "280px" });
  });

  it("does not reposition for scroll events from inside the popup", () => {
    const { trigger } = renderPicker();
    const getBoundingClientRect = vi.spyOn(trigger, "getBoundingClientRect");

    fireEvent.click(trigger);
    getBoundingClientRect.mockClear();
    fireEvent.scroll(screen.getByRole("listbox"));
    expect(getBoundingClientRect).not.toHaveBeenCalled();

    fireEvent.scroll(document);
    expect(getBoundingClientRect).toHaveBeenCalled();
  });

  it("cleans up a pending typeahead timer when unmounted", () => {
    vi.useFakeTimers();
    const clearTimeout = vi.spyOn(globalThis, "clearTimeout");
    const { trigger, unmount } = renderPicker();

    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: "g" });
    unmount();

    expect(clearTimeout).toHaveBeenCalled();
  });

  it("keeps listbox and active option IDs isolated across pickers", () => {
    const onChange = vi.fn();
    render(
      <div className="theme-dark">
        <GatewayPicker options={options} value="pixel:sim-1" placeholder="Choose phone" onChange={onChange} />
        <GatewayPicker options={options} value="galaxy:sim-3" placeholder="Choose phone" onChange={onChange} />
      </div>,
    );
    const [firstPicker, secondPicker] = screen.getAllByRole("combobox", { name: "Phone" });

    fireEvent.keyDown(firstPicker, { key: "ArrowDown" });
    const firstListboxId = firstPicker.getAttribute("aria-controls");
    const firstActiveOptionId = firstPicker.getAttribute("aria-activedescendant");
    fireEvent.keyDown(secondPicker, { key: "ArrowDown" });

    expect(firstListboxId).not.toBe(secondPicker.getAttribute("aria-controls"));
    expect(firstActiveOptionId).not.toBe(secondPicker.getAttribute("aria-activedescendant"));
    expect(document.getElementById(secondPicker.getAttribute("aria-activedescendant") ?? "")).toBeInTheDocument();
  });

  it("scrolls the initially selected option into view when the popup mounts", () => {
    const original = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView");
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });
    try {
      const { trigger } = renderPicker({ value: "galaxy:sim-3" });
      fireEvent.click(trigger);

      expect(scrollIntoView).toHaveBeenCalled();
      const selected = screen.getByRole("option", { name: /Galaxy S24/ });
      expect(scrollIntoView.mock.contexts).toContain(selected);
    } finally {
      if (original) Object.defineProperty(HTMLElement.prototype, "scrollIntoView", original);
      else Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
    }
  });

  it("does not open when the picker itself is disabled or without choices", () => {
    const { trigger } = renderPicker({ disabled: true });
    fireEvent.click(trigger);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    cleanup();
    const { trigger: emptyTrigger } = renderPicker({ options: [] });
    fireEvent.click(emptyTrigger);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });
});
