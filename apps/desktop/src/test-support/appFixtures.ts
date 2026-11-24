import { fireEvent, screen } from "@testing-library/react";
import { onTestFinished, vi } from "vitest";
import type { GatewayView } from "../bridge/bridge";

export const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
};

export const stubClipboard = () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  onTestFinished(() => {
    delete (navigator as { clipboard?: unknown }).clipboard;
  });
  return writeText;
};
export const message = () => screen.getByLabelText("Message") as HTMLTextAreaElement;
export const type = (value: string) =>
  fireEvent.change(message(), { target: { value } });
export const routeValue = (gateway: GatewayView) =>
  `${encodeURIComponent(gateway.id)} ${encodeURIComponent(gateway.simId)}`;
export const chooseGateway = async (gateway: GatewayView) => {
  fireEvent.click(screen.getByRole("combobox", { name: "Phone" }));
  const option = (await screen.findAllByRole("option")).find(
    (candidate) => candidate.getAttribute("data-gateway-choice") === routeValue(gateway),
  );
  if (!option) throw new Error(`Gateway choice was not rendered: ${routeValue(gateway)}`);
  fireEvent.click(option);
};
export const openComposerWindow = (conversationId: string, head = false) =>
  window.history.replaceState(
    {},
    "",
    `/?window=composer&conversationId=${conversationId}${head ? "&head=1" : ""}`,
  );

export class TestResizeObserver {
  static observers: TestResizeObserver[] = [];
  readonly targets = new Set<Element>();

  constructor(private readonly callback: ResizeObserverCallback) {
    TestResizeObserver.observers.push(this);
  }

  observe(target: Element) {
    this.targets.add(target);
  }

  disconnect() {
    this.targets.clear();
  }

  static resize(...targets: Element[]) {
    for (const observer of TestResizeObserver.observers) {
      const entries = targets
        .filter((target) => observer.targets.has(target))
        .map((target) => ({ target } as ResizeObserverEntry));
      if (entries.length) observer.callback(entries, observer as unknown as ResizeObserver);
    }
  }

  static reset() {
    TestResizeObserver.observers = [];
  }
}
export const mockMessageListGeometry = (geometry: { overlay: number; viewport: number; scrollHeight?: number }) => {
  const offsetHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
  const clientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
  const scrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
  Object.defineProperties(HTMLElement.prototype, {
    offsetHeight: {
      configurable: true,
      get() { return this.id === "composer-area" ? geometry.overlay : 0; },
    },
    clientHeight: {
      configurable: true,
      get() { return this.id === "message-list" ? geometry.viewport : 0; },
    },
    scrollHeight: {
      configurable: true,
      get() { return this.id === "message-list" ? geometry.scrollHeight ?? 1000 : 0; },
    },
  });
  return () => {
    for (const [name, descriptor] of [["offsetHeight", offsetHeight], ["clientHeight", clientHeight], ["scrollHeight", scrollHeight]] as const) {
      if (descriptor) Object.defineProperty(HTMLElement.prototype, name, descriptor);
      else delete (HTMLElement.prototype as unknown as Record<string, unknown>)[name];
    }
  };
};
