import { act, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useMaximizeButtonRegion } from "./useMaximizeButtonRegion";

const bridge = () => ({
  set_maximize_button_region: vi.fn().mockResolvedValue(undefined),
  subscribe_maximize_button_hover: vi.fn((_listener: (hovered: boolean) => void) => () => undefined),
});

function Region({ nativeBridge, enabled = true }: { nativeBridge: ReturnType<typeof bridge>; enabled?: boolean }) {
  useMaximizeButtonRegion({ bridge: nativeBridge as never, enabled });
  return <header id="desktop-titlebar"><button id="window-maximize" type="button" /></header>;
}

function NoMaximizeButton({ nativeBridge }: { nativeBridge: ReturnType<typeof bridge> }) {
  useMaximizeButtonRegion({ bridge: nativeBridge as never, enabled: true });
  return <header id="desktop-titlebar" />;
}

describe("useMaximizeButtonRegion", () => {
  const originalDevicePixelRatio = window.devicePixelRatio;

  afterEach(() => {
    vi.unstubAllGlobals();
    Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: originalDevicePixelRatio });
  });

  it("sends rounded, deduplicated regions and clears them on cleanup", async () => {
    const nativeBridge = bridge();
    const observe = vi.fn();
    const disconnect = vi.fn();
    vi.stubGlobal("ResizeObserver", class { observe = observe; disconnect = disconnect; });
    vi.stubGlobal("matchMedia", vi.fn(() => ({ addEventListener: vi.fn(), removeEventListener: vi.fn() })));
    const { unmount } = render(<Region nativeBridge={nativeBridge} />);
    const button = document.getElementById("window-maximize")!;
    vi.spyOn(button, "getBoundingClientRect").mockReturnValue({ x: 10.4, y: 20.6, width: 45.5, height: 31.4 } as DOMRect);

    await act(async () => window.dispatchEvent(new Event("resize")));
    await act(async () => window.dispatchEvent(new Event("resize")));
    expect(nativeBridge.set_maximize_button_region).toHaveBeenCalledWith({ x: 10, y: 21, width: 46, height: 31 });
    expect(nativeBridge.set_maximize_button_region).toHaveBeenCalledTimes(2);
    expect(observe).toHaveBeenCalledTimes(2);

    unmount();
    expect(disconnect).toHaveBeenCalledOnce();
    expect(nativeBridge.set_maximize_button_region).toHaveBeenLastCalledWith(null);
  });

  it("re-sends the region on devicePixelRatio change even when the rect is unchanged", async () => {
    const nativeBridge = bridge();
    let dprListener: (() => void) | undefined;
    const matchMedia = vi.fn(() => ({
      addEventListener: vi.fn((_type: string, handler: () => void) => { dprListener = handler; }),
      removeEventListener: vi.fn(),
    }));
    vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
    vi.stubGlobal("matchMedia", matchMedia);
    const { unmount } = render(<Region nativeBridge={nativeBridge} />);
    const region = { x: 10, y: 20, width: 40, height: 30 };
    vi.spyOn(document.getElementById("window-maximize")!, "getBoundingClientRect").mockReturnValue(
      { x: 10, y: 20, width: 40, height: 30 } as DOMRect,
    );

    await act(async () => window.dispatchEvent(new Event("resize")));
    expect(nativeBridge.set_maximize_button_region).toHaveBeenCalledTimes(2);

    Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: 2 });
    await act(async () => dprListener?.());
    expect(matchMedia).toHaveBeenLastCalledWith("(resolution: 2dppx)");
    expect(nativeBridge.set_maximize_button_region).toHaveBeenCalledTimes(3);
    expect(nativeBridge.set_maximize_button_region).toHaveBeenLastCalledWith(region);
    unmount();
  });

  it("sends null when the maximize button is absent", () => {
    const nativeBridge = bridge();
    render(<NoMaximizeButton nativeBridge={nativeBridge} />);
    expect(nativeBridge.set_maximize_button_region).toHaveBeenCalledWith(null);
  });

  it("clears native hover when the window loses focus", () => {
    const nativeBridge = bridge();
    render(<Region nativeBridge={nativeBridge} />);
    const hover = nativeBridge.subscribe_maximize_button_hover.mock.calls[0]?.[0];
    if (!hover) throw new Error("Expected native hover subscription");
    act(() => hover(true));
    expect(document.getElementById("window-maximize")?.hasAttribute("data-native-hover")).toBe(true);
    act(() => window.dispatchEvent(new Event("blur")));
    expect(document.getElementById("window-maximize")?.hasAttribute("data-native-hover")).toBe(false);
  });

  it("tracks native hover without subscribing for disabled hosts", () => {
    const nativeBridge = bridge();
    const { rerender } = render(<Region nativeBridge={nativeBridge} enabled={false} />);
    expect(nativeBridge.subscribe_maximize_button_hover).not.toHaveBeenCalled();

    rerender(<Region nativeBridge={nativeBridge} />);
    const hover = nativeBridge.subscribe_maximize_button_hover.mock.calls[0]?.[0];
    if (!hover) throw new Error("Expected native hover subscription");
    act(() => hover(true));
    expect(document.getElementById("window-maximize")?.hasAttribute("data-native-hover")).toBe(true);
    act(() => hover(false));
    expect(document.getElementById("window-maximize")?.hasAttribute("data-native-hover")).toBe(false);
  });
});
