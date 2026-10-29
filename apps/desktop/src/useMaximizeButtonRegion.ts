import { useEffect } from "react";
import type { MaximizeButtonRegion } from "@peppy/desktop-ui/bridge-contracts";
import type { DesktopBridge } from "./bridge";

type UseMaximizeButtonRegionOptions = {
  bridge: DesktopBridge;
  enabled: boolean;
};

const maximizeButtonRegion = (button: HTMLElement): MaximizeButtonRegion => {
  const { x, y, width, height } = button.getBoundingClientRect();
  return { x: Math.round(x), y: Math.round(y), width: Math.round(width), height: Math.round(height) };
};

const currentRegion = (): MaximizeButtonRegion | null => {
  const button = document.getElementById("window-maximize");
  return button instanceof HTMLElement ? maximizeButtonRegion(button) : null;
};

// `undefined` means nothing has been sent yet, so a first `null` must still go out.
const sameRegion = (left: MaximizeButtonRegion | null | undefined, right: MaximizeButtonRegion | null) => {
  if (left === undefined || left === null || right === null) return left === right;
  return left.x === right.x && left.y === right.y && left.width === right.width && left.height === right.height;
};

export function useMaximizeButtonRegion({ bridge, enabled }: UseMaximizeButtonRegionOptions): void {
  useEffect(() => {
    if (!enabled) return;

    let active = true;
    let previousRegion: MaximizeButtonRegion | null | undefined;
    let media: MediaQueryList | undefined;
    let observer: ResizeObserver | undefined;
    const sendRegion = (region: MaximizeButtonRegion | null, force = false) => {
      if (!force && sameRegion(previousRegion, region)) return;
      previousRegion = region;
      // Region updates are best-effort: the native side validates them, and a later layout change resends.
      void bridge.set_maximize_button_region(region).catch(() => undefined);
    };
    const measure = () => sendRegion(currentRegion());
    const clearNativeHover = () => {
      document.getElementById("window-maximize")?.removeAttribute("data-native-hover");
    };
    const observeDevicePixelRatio = () => {
      media?.removeEventListener("change", handleDevicePixelRatioChange);
      media = window.matchMedia?.(`(resolution: ${window.devicePixelRatio}dppx)`);
      media?.addEventListener("change", handleDevicePixelRatioChange);
    };
    const handleDevicePixelRatioChange = () => {
      observeDevicePixelRatio();
      // The CSS rect can stay identical while the physical size changes, so bypass the dedupe.
      sendRegion(currentRegion(), true);
    };
    const button = document.getElementById("window-maximize");
    const titlebar = document.getElementById("desktop-titlebar");
    if (typeof ResizeObserver !== "undefined") {
      observer = new ResizeObserver(measure);
      if (button) observer.observe(button);
      if (titlebar) observer.observe(titlebar);
    }
    const unsubscribe = bridge.subscribe_maximize_button_hover((hovered) => {
      if (!active) return;
      const maximizeButton = document.getElementById("window-maximize");
      if (!(maximizeButton instanceof HTMLElement)) return;
      maximizeButton.toggleAttribute("data-native-hover", hovered);
    });

    window.addEventListener("resize", measure);
    // Safeguard for a native leave event that never arrives when the window loses focus.
    window.addEventListener("blur", clearNativeHover);
    observeDevicePixelRatio();
    measure();

    return () => {
      active = false;
      observer?.disconnect();
      media?.removeEventListener("change", handleDevicePixelRatioChange);
      window.removeEventListener("resize", measure);
      window.removeEventListener("blur", clearNativeHover);
      unsubscribe();
      clearNativeHover();
      sendRegion(null, true);
    };
  }, [bridge, enabled]);
}
