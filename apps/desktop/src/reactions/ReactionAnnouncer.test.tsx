import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { type Announcement, ReactionAnnouncer } from "./ReactionLayer";

afterEach(cleanup);

describe("ReactionAnnouncer", () => {
  it("renders both regions with no text, so they exist before anything is announced", () => {
    const { container } = render(<ReactionAnnouncer announcement={{ seq: 0, polite: "", alert: "" }} />);
    expect(container.querySelector("#reaction-announcer")).toHaveAttribute("role", "status");
    expect(container.querySelector("#reaction-alert")).toHaveAttribute("aria-live", "assertive");
  });

  it("sets a polite line, and clears then sets a repeated line so it is announced again", async () => {
    const first: Announcement = { seq: 1, polite: "Sending your 😮 reaction.", alert: "" };
    const { container, rerender } = render(<ReactionAnnouncer announcement={first} />);
    await waitFor(() => expect(container.querySelector("#reaction-announcer")).toHaveTextContent("Sending your 😮 reaction."));

    // A new announcement object with the same text: the region goes empty first, then the text comes back.
    rerender(<ReactionAnnouncer announcement={{ ...first, seq: 2 }} />);
    expect(container.querySelector("#reaction-announcer")).toHaveTextContent("");
    await act(async () => {});
    await waitFor(() => expect(container.querySelector("#reaction-announcer")).toHaveTextContent("Sending your 😮 reaction."));
  });

  it("puts failures in the assertive region, leaving the polite region empty", async () => {
    const { container } = render(<ReactionAnnouncer announcement={{ seq: 3, polite: "", alert: "Couldn't reach Peppy. Try again." }} />);
    await waitFor(() => expect(container.querySelector("#reaction-alert")).toHaveTextContent("Couldn't reach Peppy. Try again."));
    expect(container.querySelector("#reaction-announcer")).toHaveTextContent("");
  });
});
