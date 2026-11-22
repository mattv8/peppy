import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { FeedbackSettings, feedbackPageUrl } from "./FeedbackSettings";

afterEach(() => {
  cleanup();
});

function desktopBridge(origin: string | undefined, openExternalUrl: ((url: string) => Promise<void>) | undefined = vi.fn().mockResolvedValue(undefined)) {
  return { hosted_origin_url: vi.fn().mockResolvedValue(origin), open_external_url: openExternalUrl };
}

describe("feedbackPageUrl", () => {
  it.each([
    ["https://peppy.pro", "https://peppy.pro/feedback"],
    ["https://peppy.pro/account", "https://peppy.pro/feedback"],
    ["http://127.0.0.1:8080", "http://127.0.0.1:8080/feedback"],
    ["http://localhost:3000", "http://localhost:3000/feedback"],
    ["http://[::1]:8080", "http://[::1]:8080/feedback"],
  ])("resolves %s", (origin, expected) => {
    expect(feedbackPageUrl(origin)).toBe(expected);
  });

  it.each([
    ["undefined", undefined],
    ["an empty string", ""],
    ["an unparseable value", "not a url"],
    ["plain http on a public host", "http://evil.example"],
    ["a javascript URL", "javascript:alert(1)"],
    ["credentials in the origin", "https://user:pw@peppy.pro"],
  ])("refuses %s", (_case, origin) => {
    expect(feedbackPageUrl(origin)).toBeUndefined();
  });
});

describe("FeedbackSettings on desktop", () => {
  it("opens exactly the feedback page on the hosted origin", async () => {
    const bridge = desktopBridge("https://peppy.pro");
    render(<FeedbackSettings browserHost={false} bridge={bridge} />);

    fireEvent.click(screen.getByRole("button", { name: /send feedback/i }));

    await vi.waitFor(() => expect(bridge.open_external_url).toHaveBeenCalledWith("https://peppy.pro/feedback"));
    expect(bridge.open_external_url).toHaveBeenCalledOnce();
  });

  it("describes the link with the sign-in note and marks its section", () => {
    const { container } = render(<FeedbackSettings browserHost={false} bridge={desktopBridge("https://peppy.pro")} />);

    const button = screen.getByRole("button", { name: /send feedback/i });
    expect(button).toHaveAccessibleDescription(peppyCopy.settings_feedback_note);
    expect(container.querySelector("#settings-feedback-title")).toHaveTextContent(peppyCopy.settings_feedback_title);
    expect(container.querySelector("div#settings-feedback")).toHaveAttribute("data-settings-subsection", "feedback");
  });

  it("shows a fixed error when opening fails and clears it on a later successful open", async () => {
    const openExternalUrl = vi.fn()
      .mockRejectedValueOnce(new Error("sensitive native failure"))
      .mockResolvedValueOnce(undefined);
    render(<FeedbackSettings browserHost={false} bridge={desktopBridge("https://peppy.pro", openExternalUrl)} />);

    fireEvent.click(screen.getByRole("button", { name: /send feedback/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.settings_feedback_open_failed);
    expect(screen.queryByText("sensitive native failure")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /send feedback/i }));

    await vi.waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(openExternalUrl).toHaveBeenCalledTimes(2);
  });

  it("opens once and disables the button while the first open is in flight", async () => {
    let finishOpen!: () => void;
    const openExternalUrl = vi.fn(() => new Promise<void>(resolve => { finishOpen = resolve; }));
    render(<FeedbackSettings browserHost={false} bridge={desktopBridge("https://peppy.pro", openExternalUrl)} />);
    const button = screen.getByRole("button", { name: /send feedback/i });

    fireEvent.click(button);
    fireEvent.click(button);

    await vi.waitFor(() => expect(openExternalUrl).toHaveBeenCalledOnce());
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("aria-busy", "true");

    await act(async () => finishOpen());

    await vi.waitFor(() => expect(button).toBeEnabled());
    expect(button).not.toHaveAttribute("aria-busy");
    expect(openExternalUrl).toHaveBeenCalledOnce();
  });

  it.each([
    ["an insecure public origin", "http://evil.example"],
    ["a javascript origin", "javascript:alert(1)"],
    ["an origin with credentials", "https://user:pw@peppy.pro"],
  ])("refuses to open the page for %s", async (_case, origin) => {
    const bridge = desktopBridge(origin);
    render(<FeedbackSettings browserHost={false} bridge={bridge} />);

    fireEvent.click(screen.getByRole("button", { name: /send feedback/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.settings_feedback_open_failed);
    expect(bridge.open_external_url).not.toHaveBeenCalled();
  });

  it("shows the error when the bridge has no opener", async () => {
    render(<FeedbackSettings browserHost={false} bridge={{ hosted_origin_url: vi.fn().mockResolvedValue("https://peppy.pro") }} />);

    fireEvent.click(screen.getByRole("button", { name: /send feedback/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.settings_feedback_open_failed);
  });

  it("opens a loopback development origin over HTTP", async () => {
    const bridge = desktopBridge("http://127.0.0.1:8080");
    render(<FeedbackSettings browserHost={false} bridge={bridge} />);

    fireEvent.click(screen.getByRole("button", { name: /send feedback/i }));

    await vi.waitFor(() => expect(bridge.open_external_url).toHaveBeenCalledWith("http://127.0.0.1:8080/feedback"));
  });
});

describe("FeedbackSettings in the browser", () => {
  it("links to the feedback page on the account origin in a new tab", () => {
    render(<FeedbackSettings browserHost accountUrl="https://peppy.pro/account" bridge={desktopBridge(undefined)} />);

    const link = screen.getByRole("link", { name: /send feedback/i });
    expect(link).toHaveAttribute("href", "https://peppy.pro/feedback");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    expect(link).toHaveAccessibleDescription(peppyCopy.settings_feedback_note);
  });

  it.each([
    ["no account URL", undefined],
    ["an invalid account URL", "not a url"],
    ["an insecure public account URL", "http://evil.example/account"],
  ])("renders nothing for %s", (_case, accountUrl) => {
    const { container } = render(<FeedbackSettings browserHost accountUrl={accountUrl} bridge={desktopBridge(undefined)} />);

    expect(container).toBeEmptyDOMElement();
  });
});
