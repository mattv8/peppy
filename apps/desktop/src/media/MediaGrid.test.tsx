import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MediaGrid } from "./MediaGrid";

const message = (id: string, body = "") => ({ id, revision: "1", sender: "self" as const, body, timestamp: `2026-01-01T12:0${id}:00.000Z`, attachments: [{ id: `${id}-image`, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, mediaMeta: { thumbnail: { id: `${id}-thumbnail`, name: `${id}-thumbnail`, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, previewUrl: "thumb" } } }] });

describe("MediaGrid", () => {
  it("renders every caption and keeps a single message-row hook per message", () => {
    const view = render(<MediaGrid messages={[message("1", "first"), message("2", "final")]} onOpen={vi.fn()} renderMessageMeta={(value) => value.id} />);
    expect(view.container.querySelectorAll("[data-message-meta]")).toHaveLength(2);
    expect(screen.getByText("first")).toBeInTheDocument();
    expect(screen.getByText("final")).toBeInTheDocument();
    expect(view.container.querySelectorAll('[data-message-row="1"]')).toHaveLength(1);
    expect(view.container.querySelector('[data-message-group="1"]')).toBeInTheDocument();
  });
});
