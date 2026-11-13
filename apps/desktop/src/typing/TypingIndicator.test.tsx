import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { expect, it } from "vitest";
import { TypingRow } from "./TypingIndicator";

it("renders a direct typing bubble without a sender avatar", () => {
  const { container } = render(<TypingRow scope="direct" label="Typing" typers={[]} showSenderAvatars={false} conversationToneSeed="aurora" toneOverrides={{}} />);
  const indicator = container.querySelector("#typing-indicator")!;
  expect(indicator).toHaveAttribute("aria-hidden", "true");
  expect(indicator).toHaveAttribute("data-typing-row");
  expect(indicator).toHaveAttribute("data-typing-scope", "direct");
  expect(container.querySelector("[data-sender-avatar]")).toBeNull();
  expect(container.querySelector("[data-typing-bubble]")).toHaveAttribute("data-peppy-tone");
  expect(container.querySelector("#typing-indicator-label")).toBeNull();
});

it("uses one group typer's avatar and neutralizes multiple typers", () => {
  const { container, rerender } = render(<TypingRow scope="group" label="Ana is typing" typers={[{ address: "+15555550142", name: "Ana", toneSeed: "ana" }]} showSenderAvatars conversationToneSeed="group" toneOverrides={{}} />);
  expect(container.querySelector('[data-sender-avatar="+15555550142"]')).toBeInTheDocument();
  expect(container.querySelector("[data-typing-bubble]")).toHaveAttribute("data-peppy-tone");
  expect(container.querySelector("#typing-indicator-label")).toHaveTextContent("Ana is typing");

  rerender(<TypingRow scope="group" label="Ana and Bea are typing" typers={[{ address: "+15555550142", name: "Ana", toneSeed: "ana" }, { address: "+15555550199", name: "Bea", toneSeed: "bea" }]} showSenderAvatars conversationToneSeed="group" toneOverrides={{}} />);
  expect(container.querySelector("[data-sender-avatar]")).toBeNull();
  expect(container.querySelector(".message-sender-gutter")).toBeInTheDocument();
  expect(container.querySelector("[data-typing-bubble]")).not.toHaveAttribute("data-peppy-tone");
});
