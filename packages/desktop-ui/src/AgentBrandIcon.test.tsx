import "@testing-library/jest-dom/vitest";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { AGENT_BRAND_IDS } from "./agent-icons/brands";
import { AgentBrandIcon } from "./AgentBrandIcon";

afterEach(() => {
  cleanup();
});

describe("AgentBrandIcon", () => {
  it.each(AGENT_BRAND_IDS)("renders a decorative inline svg for %s", (clientId) => {
    const { container } = render(<AgentBrandIcon clientId={clientId} />);
    const svg = container.querySelector("svg");

    expect(svg).not.toBeNull();
    expect(svg).toHaveAttribute("aria-hidden", "true");
    expect(svg).toHaveAttribute("focusable", "false");
    expect(svg).toHaveAttribute("data-agent-icon", clientId);
    expect(svg).toHaveAttribute("fill", "currentColor");
    expect(svg).toHaveAttribute("width", "16");
    expect(svg).toHaveAttribute("height", "16");
    expect(svg?.querySelectorAll("path").length).toBeGreaterThan(0);
  });

  it("uses the continue viewBox", () => {
    const { container } = render(<AgentBrandIcon clientId="continue" />);
    expect(container.querySelector("svg")).toHaveAttribute("viewBox", "82 82 146 136");
  });

  it("uses the 24-unit viewBox for lobe-icons brands", () => {
    const { container } = render(<AgentBrandIcon clientId="codex" />);
    expect(container.querySelector("svg")).toHaveAttribute("viewBox", "0 0 24 24");
  });

  it("honours custom size and className", () => {
    const { container } = render(<AgentBrandIcon clientId="cursor" className="x-icon" size={24} />);
    const svg = container.querySelector("svg");
    expect(svg).toHaveClass("x-icon");
    expect(svg).toHaveAttribute("width", "24");
    expect(svg).toHaveAttribute("height", "24");
  });
});
