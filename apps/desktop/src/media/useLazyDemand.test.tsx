import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionsProvider } from "./MediaActionsContext";
import { useLazyDemand } from "./useLazyDemand";
import { useRef } from "react";

const actions = () => ({ publicCopies: {}, onSave: vi.fn().mockResolvedValue("saved"), onPublish: vi.fn().mockResolvedValue(null), onRetry: vi.fn().mockResolvedValue(undefined), requestDownload: vi.fn().mockResolvedValue(undefined), loadOriginal: vi.fn().mockResolvedValue({ state: "pending" }), releaseMedia: vi.fn(), canCopyImage: false });
let callback: IntersectionObserverCallback | undefined;
const disconnect = vi.fn();

class Observer {
  constructor(next: IntersectionObserverCallback) { callback = next; }
  observe() {}
  disconnect = disconnect;
  unobserve() {}
  takeRecords() { return []; }
  readonly root = null;
  readonly rootMargin = "300px";
  readonly thresholds = [];
}

function Demand({ ids }: { ids: readonly (string | undefined)[] }) {
  const ref = useRef<HTMLDivElement>(null);
  useLazyDemand(ref, ids);
  return <div ref={ref} />;
}

afterEach(() => { callback = undefined; disconnect.mockClear(); cleanup(); vi.unstubAllGlobals(); });

describe("useLazyDemand", () => {
  it("requests defined ids after its observer intersects and disconnects", () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = actions();
    render(<MediaActionsProvider {...props}><Demand ids={["one", undefined, "two"]} /></MediaActionsProvider>);
    callback?.([{ isIntersecting: true }] as IntersectionObserverEntry[], {} as IntersectionObserver);
    expect(props.requestDownload).toHaveBeenCalledTimes(2);
    expect(disconnect).toHaveBeenCalled();
  });

  it("disconnects an unused observer on unmount", () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = actions();
    const view = render(<MediaActionsProvider {...props}><Demand ids={["one"]} /></MediaActionsProvider>);
    view.unmount();
    expect(disconnect).toHaveBeenCalled();
  });

  it("requests immediately when IntersectionObserver is unavailable", () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    const props = actions();
    render(<MediaActionsProvider {...props}><Demand ids={["one"]} /></MediaActionsProvider>);
    expect(props.requestDownload).toHaveBeenCalledWith("one");
  });

  it("does nothing without a provider or demand ids", () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    render(<Demand ids={[undefined]} />);
    expect(callback).toBeUndefined();
  });
});
