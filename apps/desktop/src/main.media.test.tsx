import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

vi.mock("react-dom/client", () => ({ createRoot: () => ({ render: vi.fn() }) }));
vi.mock("./App", () => ({ App: () => <div data-testid="app" /> }));

import { DesktopRoot } from "./main";

afterEach(cleanup);

it("does not render App for an invalid media window route", () => {
  render(<DesktopRoot search="?window=media&conversationId=bad!&attachmentId=file&messageId=message" />);
  expect(screen.getByRole("alert")).toHaveTextContent("Couldn't open this media");
  expect(screen.queryByTestId("app")).not.toBeInTheDocument();
});
