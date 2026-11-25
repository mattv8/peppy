import { render, waitFor, type RenderOptions } from "@testing-library/react";
import { App } from "../App";

/** Waits for a populated conversation header after loading, then any caller-specific signal. */
export async function renderApp(options: { loaded?: () => Promise<unknown>; renderOptions?: RenderOptions } = {}) {
  const view = render(<App />, options.renderOptions);
  await waitFor(() => {
    if (!view.container.querySelector("[data-header-title]")?.textContent?.trim()) throw new Error("Conversation header is not loaded");
  });
  await options.loaded?.();
  return view;
}
