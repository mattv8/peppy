import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WebhookEditorModal, type WebhookEditorModalProps } from "./WebhookEditorModal";
import { bridge } from "../bridge/bridge";
import { homeAssistant, zapier } from "./webhookTestFixtures";
import type { WebhookView } from "@peppy/desktop-ui/bridge-contracts";

vi.mock("../bridge/bridge", { spy: true });

const NEW_ID = "00000000-0000-4000-8000-000000000009";
const created: WebhookView = { ...homeAssistant, id: NEW_ID, name: "Ops", nameIsDerived: false, url: "https://hooks.example.com/ops", paused: false };
const jordan = {
  address: "+12025550199",
  bookId: "book-2",
  displayName: "Jordan Lee",
  number: "(202) 555-0199",
  label: "mobile",
  normalized: true,
  contactId: "contact-jordan",
  phoneId: "phone-jordan",
};
const jordanWork = { ...jordan, bookId: "book-work", label: "work", contactId: "contact-jordan-work", phoneId: "phone-jordan-work" };

function renderEditor(props: Partial<WebhookEditorModalProps> = {}) {
  const onChanged = vi.fn();
  const onClose = vi.fn();
  const opener = { current: null };
  const result = render(
    <WebhookEditorModal serverOffline={false} opener={opener} onChanged={onChanged} onClose={onClose} {...props} />,
  );
  return { ...result, onChanged, onClose };
}

const secretValue = () => (screen.getByLabelText("Signing secret") as HTMLInputElement).value;
const typeInto = (label: string, value: string) => fireEvent.change(screen.getByLabelText(label), { target: { value } });
const formElement = () => document.getElementById("webhook-editor-form") as HTMLFormElement;
const testNote = () => document.getElementById("webhook-test-note");

/** Fills a valid create form in Anyone mode and submits it through the Create button. */
async function createWithUrl(url = "https://hooks.example.com/in") {
  typeInto("Endpoint URL", url);
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await screen.findByRole("button", { name: "Close" });
}

describe("WebhookEditorModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // The server echoes what was sent, as the real create does.
    vi.mocked(bridge.webhook_create).mockImplementation(async (input) => ({
      webhook: {
        ...created,
        name: input.name ?? "",
        nameIsDerived: input.name === null,
        url: input.url,
        filterMode: input.filterMode,
        filter: input.filter,
        paused: input.paused,
      },
      secret: "whsec_new_secret",
    }));
    vi.mocked(bridge.webhook_update).mockImplementation(async (_id, input) => ({ ...homeAssistant, ...input, name: input.name ?? "" }));
    vi.mocked(bridge.webhook_reveal_secret).mockResolvedValue({ id: homeAssistant.id, secret: "whsec_real_secret" });
    vi.mocked(bridge.search_contact_recipients).mockResolvedValue([]);
  });

  afterEach(() => {
    cleanup();
    Reflect.deleteProperty(navigator, "clipboard");
    vi.restoreAllMocks();
  });

  describe("validation and filter mode", () => {
    it("requires a valid URL before creating and focuses the field", async () => {
      renderEditor();

      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      expect(await screen.findByRole("alert")).toHaveTextContent("Enter a valid URL.");
      await waitFor(() => expect(screen.getByLabelText("Endpoint URL")).toHaveFocus());
      expect(screen.getByLabelText("Endpoint URL")).toHaveAttribute("aria-invalid", "true");
      expect(bridge.webhook_create).not.toHaveBeenCalled();
    });

    it("gives the name and URL inputs accessible names", () => {
      renderEditor();

      expect(screen.getByRole("textbox", { name: "Name (optional)" })).toBeInTheDocument();
      expect(screen.getByRole("textbox", { name: "Endpoint URL" })).toBeInTheDocument();
    });

    it("suggests the host as the name placeholder", () => {
      renderEditor();

      typeInto("Endpoint URL", "https://www.hooks.example.com/in");

      expect(screen.getByLabelText("Name (optional)")).toHaveAttribute("placeholder", "hooks.example.com");
    });

    it("hides the contact chips for Anyone and shows them for the filtered modes", () => {
      renderEditor();

      expect(screen.queryByLabelText("Contacts or numbers")).not.toBeInTheDocument();

      fireEvent.click(screen.getByRole("radio", { name: "Only these contacts" }));
      expect(screen.getByLabelText("Contacts or numbers")).toBeInTheDocument();
      expect(screen.getByText("Pick contacts or type a phone number. A contact chip matches all of that contact's numbers.")).toBeVisible();

      fireEvent.click(screen.getByRole("radio", { name: "Anyone" }));
      expect(screen.queryByLabelText("Contacts or numbers")).not.toBeInTheDocument();
    });

    it("moves the selection and focus with arrow, Home and End keys", () => {
      renderEditor();

      const anyone = screen.getByRole("radio", { name: "Anyone" });
      anyone.focus();
      fireEvent.keyDown(anyone, { key: "ArrowRight" });

      const only = screen.getByRole("radio", { name: "Only these contacts" });
      expect(only).toHaveAttribute("aria-checked", "true");
      expect(only).toHaveFocus();
      expect(anyone).toHaveAttribute("aria-checked", "false");

      fireEvent.keyDown(only, { key: "End" });
      expect(screen.getByRole("radio", { name: "Everyone except" })).toHaveAttribute("aria-checked", "true");

      fireEvent.keyDown(screen.getByRole("radio", { name: "Everyone except" }), { key: "Home" });
      expect(anyone).toHaveAttribute("aria-checked", "true");
    });

    it("requires at least one contact or number for a filtered webhook", async () => {
      renderEditor();
      typeInto("Endpoint URL", "https://hooks.example.com/in");
      fireEvent.click(screen.getByRole("radio", { name: "Only these contacts" }));

      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      expect(await screen.findByRole("alert")).toHaveTextContent("Add at least one contact or number.");
      expect(screen.getByLabelText("Contacts or numbers")).toHaveAttribute("aria-invalid", "true");
      expect(bridge.webhook_create).not.toHaveBeenCalled();
    });

    it("names the filtered contacts list for assistive tech", () => {
      renderEditor({ webhook: zapier });

      expect(screen.getByRole("list", { name: "Filtered contacts" })).toHaveTextContent("Alice Chen");
    });
  });

  describe("filter entries", () => {
    it("stores a typed number as an address-only entry", async () => {
      renderEditor();
      typeInto("Endpoint URL", "https://hooks.example.com/in");
      fireEvent.click(screen.getByRole("radio", { name: "Only these contacts" }));
      const chips = screen.getByLabelText("Contacts or numbers");
      fireEvent.change(chips, { target: { value: "+1 202 555 0127" } });
      fireEvent.keyDown(chips, { key: "Enter" });

      expect(await screen.findByText("(202) 555-0127")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      await waitFor(() => expect(bridge.webhook_create).toHaveBeenCalledWith({
        name: null,
        url: "https://hooks.example.com/in",
        events: ["message.received"],
        filterMode: "only",
        filter: [{ address: "+12025550127", label: null, contactId: null, bookId: null }],
        paused: false,
      }));
    });

    it("stores a chosen contact suggestion with its contact details", async () => {
      vi.mocked(bridge.search_contact_recipients).mockResolvedValue([jordan]);
      renderEditor();
      typeInto("Endpoint URL", "https://hooks.example.com/in");
      fireEvent.click(screen.getByRole("radio", { name: "Only these contacts" }));
      const chips = screen.getByLabelText("Contacts or numbers");

      fireEvent.change(chips, { target: { value: "Jor" } });
      await screen.findByRole("option", { name: /Jordan Lee/ });
      fireEvent.keyDown(chips, { key: "Enter" });
      expect(await screen.findByText("Jordan Lee")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      await waitFor(() => expect(bridge.webhook_create).toHaveBeenCalledWith(expect.objectContaining({
        filterMode: "only",
        filter: [{ address: "+12025550199", label: "Jordan Lee", contactId: "contact-jordan", bookId: "book-2" }],
      })));
    });

    it("keeps the exact selected contact when suggestions share an address", async () => {
      vi.mocked(bridge.search_contact_recipients).mockResolvedValue([jordan, jordanWork]);
      renderEditor();
      typeInto("Endpoint URL", "https://hooks.example.com/in");
      fireEvent.click(screen.getByRole("radio", { name: "Only these contacts" }));
      const chips = screen.getByLabelText("Contacts or numbers");

      fireEvent.change(chips, { target: { value: "Jor" } });
      const suggestions = await screen.findAllByRole("option", { name: /Jordan Lee/ });
      fireEvent.mouseDown(suggestions[1]);
      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      await waitFor(() => expect(bridge.webhook_create).toHaveBeenCalledWith(expect.objectContaining({
        filter: [{ address: "+12025550199", label: "Jordan Lee", contactId: "contact-jordan-work", bookId: "book-work" }],
      })));
    });

    it("keeps a typed matching number address-only", async () => {
      vi.mocked(bridge.search_contact_recipients).mockResolvedValue([jordan]);
      renderEditor();
      typeInto("Endpoint URL", "https://hooks.example.com/in");
      fireEvent.click(screen.getByRole("radio", { name: "Only these contacts" }));
      const chips = screen.getByLabelText("Contacts or numbers");
      fireEvent.change(chips, { target: { value: "+1 202 555 0199" } });
      fireEvent.keyDown(chips, { key: "Enter" });
      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      await waitFor(() => expect(bridge.webhook_create).toHaveBeenCalledWith(expect.objectContaining({
        filter: [{ address: "+12025550199", label: null, contactId: null, bookId: null }],
      })));
    });

    it("sends the existing filter with the metadata of saved contacts on save", async () => {
      renderEditor({ webhook: zapier });

      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      await waitFor(() => expect(bridge.webhook_update).toHaveBeenCalledWith(zapier.id, {
        name: "Zapier: Alice & Bob",
        url: zapier.url,
        events: ["message.received"],
        filterMode: "only",
        filter: zapier.filter,
        paused: false,
      }));
    });
  });

  describe("create and edit", () => {
    it("creates the webhook, shows its secret, and keeps the modal open in edit mode", async () => {
      const { onChanged, onClose } = renderEditor();
      typeInto("Name (optional)", "Ops");
      typeInto("Endpoint URL", "https://hooks.example.com/ops");

      fireEvent.click(screen.getByRole("button", { name: "Create" }));

      expect(await screen.findByRole("button", { name: "Close" })).toBeInTheDocument();
      expect(bridge.webhook_create).toHaveBeenCalledWith({
        name: "Ops",
        url: "https://hooks.example.com/ops",
        events: ["message.received"],
        filterMode: "all",
        filter: [],
        paused: false,
      });
      expect(onChanged).toHaveBeenCalledWith(created);
      expect(screen.getByRole("dialog", { name: "Webhook created" })).toBeInTheDocument();
      expect(secretValue()).toBe("whsec_new_secret");
      expect(onClose).not.toHaveBeenCalled();
      expect(screen.queryByRole("button", { name: "Done" })).not.toBeInTheDocument();
    });

    it("moves focus to Copy secret after create so the new secret is reachable", async () => {
      renderEditor();
      await createWithUrl();

      await waitFor(() => expect(document.getElementById("webhook-secret-copy")).toHaveFocus());
      expect(screen.queryByText(/once/i)).not.toBeInTheDocument();
    });

    it("keeps Save active after create and updates the webhook it made", async () => {
      const { onClose } = renderEditor();
      await createWithUrl();
      typeInto("Name (optional)", "Ops renamed");

      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      await waitFor(() => expect(bridge.webhook_update).toHaveBeenCalledWith(NEW_ID, expect.objectContaining({ name: "Ops renamed" })));
      expect(bridge.webhook_create).toHaveBeenCalledTimes(1);
      await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    });

    it("confirms before closing a created webhook with unsaved edits, then closes on the second Close", async () => {
      const { onClose } = renderEditor();
      await createWithUrl();
      typeInto("Name (optional)", "Half typed");

      fireEvent.click(screen.getByRole("button", { name: "Close" }));

      expect(onClose).not.toHaveBeenCalled();
      expect(document.getElementById("webhook-discard-note")).toHaveTextContent("Unsaved changes. Save, or select Close again to discard them.");

      fireEvent.click(screen.getByRole("button", { name: "Close" }));
      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it("closes a created webhook without confirmation when nothing is unsaved", async () => {
      const { onClose } = renderEditor();
      await createWithUrl();

      fireEvent.click(screen.getByRole("button", { name: "Close" }));

      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it("says the secret is created after saving before a webhook exists", () => {
      renderEditor();

      expect(screen.getByText("Created after you save.")).toBeInTheDocument();
      expect(screen.queryByLabelText("Signing secret")).not.toBeInTheDocument();
    });

    it("saves changes to an existing webhook and closes", async () => {
      const { onChanged, onClose } = renderEditor({ webhook: homeAssistant });
      typeInto("Name (optional)", "Renamed");
      vi.mocked(bridge.webhook_update).mockResolvedValueOnce({ ...homeAssistant, name: "Renamed", nameIsDerived: false });

      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
      expect(bridge.webhook_update).toHaveBeenCalledWith(homeAssistant.id, {
        name: "Renamed",
        url: homeAssistant.url,
        events: ["message.received"],
        filterMode: "all",
        filter: [],
        paused: false,
      });
      expect(onChanged).toHaveBeenCalledWith(expect.objectContaining({ name: "Renamed" }));
    });

    it("submits from the form, so Enter in a field saves", async () => {
      renderEditor();
      typeInto("Endpoint URL", "https://hooks.example.com/in");

      fireEvent.submit(formElement());

      await waitFor(() => expect(bridge.webhook_create).toHaveBeenCalledTimes(1));
    });

    it("ignores a second submit while the first save is still running", async () => {
      vi.mocked(bridge.webhook_create).mockReturnValue(new Promise(() => {}));
      renderEditor();
      typeInto("Endpoint URL", "https://hooks.example.com/in");

      fireEvent.submit(formElement());
      fireEvent.submit(formElement());

      expect(bridge.webhook_create).toHaveBeenCalledTimes(1);
    });

    it("shows an offline note and disables saving while the server is offline", () => {
      renderEditor({ webhook: homeAssistant, serverOffline: true });

      expect(screen.getByText("Server is offline. Changes can't be saved right now.")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    });
  });

  describe("server field errors", () => {
    it("maps a URL code to the URL field, marks it invalid and focuses it", async () => {
      vi.mocked(bridge.webhook_update).mockRejectedValueOnce({ code: "webhook_url_insecure", message: "insecure" });
      const { onClose } = renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      const url = screen.getByLabelText("Endpoint URL");
      expect(await within(screen.getByLabelText("Endpoint URL").parentElement as HTMLElement).findByRole("alert")).toHaveTextContent("Use an https:// URL.");
      expect(url).toHaveAttribute("aria-invalid", "true");
      expect(url).toHaveAttribute("aria-describedby", "webhook-url-error");
      expect(url).toHaveFocus();
      expect(document.getElementById("webhook-save-error")).not.toBeInTheDocument();
      expect(onClose).not.toHaveBeenCalled();
    });

    it("maps a name code to the name field", async () => {
      vi.mocked(bridge.webhook_update).mockRejectedValueOnce({ code: "webhook_name_invalid", message: "long" });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      expect(await screen.findByText("Use 80 characters or fewer.")).toBeInTheDocument();
      expect(screen.getByLabelText("Name (optional)")).toHaveAttribute("aria-invalid", "true");
      await waitFor(() => expect(screen.getByLabelText("Name (optional)")).toHaveFocus());
    });

    it("maps a filter code to the chip input and its error", async () => {
      vi.mocked(bridge.webhook_update).mockRejectedValueOnce({ code: "webhook_filter_invalid", message: "bad" });
      renderEditor({ webhook: zapier });

      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      const chips = await screen.findByLabelText("Contacts or numbers");
      expect(chips).toHaveAttribute("aria-invalid", "true");
      expect(chips.getAttribute("aria-describedby")).toMatch(/ webhook-filter-required$/);
      expect(screen.getByText("Check the contacts and numbers in this filter.")).toHaveAttribute("id", "webhook-filter-required");
      expect(chips).toHaveFocus();
    });

    it("sends other codes to the form-level save error", async () => {
      vi.mocked(bridge.webhook_update).mockRejectedValueOnce({ code: "webhook_limit", message: "limit" });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      expect(await screen.findByRole("alert")).toHaveTextContent("You can have up to 10 webhooks.");
      expect(document.getElementById("webhook-save-error")).toHaveTextContent("You can have up to 10 webhooks.");
      expect(screen.getByLabelText("Endpoint URL")).not.toHaveAttribute("aria-invalid");
    });

    it("clears a server field error once the draft changes", async () => {
      vi.mocked(bridge.webhook_update).mockRejectedValueOnce({ code: "webhook_url_insecure", message: "insecure" });
      renderEditor({ webhook: homeAssistant });
      fireEvent.click(screen.getByRole("button", { name: "Save" }));
      await screen.findByText("Use an https:// URL.");

      typeInto("Endpoint URL", "https://hooks.example.com/fixed");

      expect(screen.queryByText("Use an https:// URL.")).not.toBeInTheDocument();
      expect(screen.getByLabelText("Endpoint URL")).not.toHaveAttribute("aria-invalid");
    });
  });

  describe("signing secret", () => {
    it("masks the secret until Reveal fetches and shows it", async () => {
      renderEditor({ webhook: homeAssistant });
      expect(secretValue()).toMatch(/^whsec_•+$/);

      fireEvent.click(screen.getByRole("button", { name: "Reveal secret" }));

      expect(await screen.findByDisplayValue("whsec_real_secret")).toBeInTheDocument();
      expect(bridge.webhook_reveal_secret).toHaveBeenCalledWith(homeAssistant.id);

      fireEvent.click(screen.getByRole("button", { name: "Hide secret" }));
      expect(secretValue()).toMatch(/^whsec_•+$/);
    });

    it("copies the secret and confirms it", async () => {
      const writeText = vi.fn().mockResolvedValue(undefined);
      Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Copy secret" }));

      expect(await screen.findByText("Copied.")).toBeInTheDocument();
      expect(writeText).toHaveBeenCalledWith("whsec_real_secret");
    });

    it("confirms rotation inline, without a native dialog, then shows the new secret and returns focus to it", async () => {
      const confirm = vi.spyOn(window, "confirm");
      vi.mocked(bridge.webhook_rotate_secret).mockResolvedValue({ id: homeAssistant.id, secret: "whsec_rotated" });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Rotate" }));

      const confirmBox = document.getElementById("webhook-rotate-confirm") as HTMLElement;
      expect(confirmBox).toHaveTextContent("Rotate secret? Receivers must be updated.");
      expect(within(confirmBox).getByRole("button", { name: "Cancel" })).toHaveFocus();
      expect(bridge.webhook_rotate_secret).not.toHaveBeenCalled();

      fireEvent.click(within(confirmBox).getByRole("button", { name: "Rotate secret" }));

      expect(await screen.findByText(/New secret created/)).toBeInTheDocument();
      expect(secretValue()).toBe("whsec_rotated");
      expect(document.getElementById("webhook-rotate-confirm")).not.toBeInTheDocument();
      await waitFor(() => expect(document.getElementById("webhook-secret-value")).toHaveFocus());
      expect(confirm).not.toHaveBeenCalled();
    });

    it("cancels rotation without calling the server and returns focus to Rotate", () => {
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Rotate" }));
      fireEvent.click(within(document.getElementById("webhook-rotate-confirm") as HTMLElement).getByRole("button", { name: "Cancel" }));

      expect(document.getElementById("webhook-rotate-confirm")).not.toBeInTheDocument();
      expect(bridge.webhook_rotate_secret).not.toHaveBeenCalled();
      expect(screen.getByRole("button", { name: "Rotate" })).toHaveFocus();
    });
  });

  describe("send test", () => {
    it("is disabled for a webhook that is not saved yet, with a visible note", () => {
      renderEditor();

      const test = screen.getByRole("button", { name: "Send test" });
      expect(test).toBeDisabled();
      expect(test).toHaveAttribute("title", "Save first to send a test");
      expect(testNote()).toHaveTextContent("Save to test");
    });

    it("is enabled for a saved webhook without unsaved changes", () => {
      renderEditor({ webhook: homeAssistant });

      expect(screen.getByRole("button", { name: "Send test" })).toBeEnabled();
      expect(testNote()).toBeNull();
    });

    it("is disabled with a visible note while the draft differs from the saved webhook", () => {
      renderEditor({ webhook: homeAssistant });

      typeInto("Name (optional)", "Changed");

      expect(screen.getByRole("button", { name: "Send test" })).toBeDisabled();
      expect(testNote()).toHaveTextContent("Save changes to test");
    });

    it("blocks saving and testing while contact text is pending", () => {
      renderEditor({ webhook: zapier });
      const chips = screen.getByLabelText("Contacts or numbers");
      fireEvent.change(chips, { target: { value: "Jordan" } });

      expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Send test" })).toBeDisabled();
      expect(document.getElementById("webhook-filter-pending")).toHaveTextContent("Press Enter to add what you typed, or clear it, before saving.");
      expect(chips.getAttribute("aria-describedby")).toMatch(/webhook-filter-pending/);
      expect(screen.getByRole("button", { name: "Save" })).toHaveAttribute("aria-describedby", "webhook-filter-pending");
      expect(testNote()).toBeNull();
    });

    it("clears a previous test result as soon as the draft is edited", async () => {
      vi.mocked(bridge.webhook_test).mockResolvedValue({ ok: true, statusCode: 200, durationMs: 142, error: null });
      renderEditor({ webhook: homeAssistant });
      fireEvent.click(screen.getByRole("button", { name: "Send test" }));
      expect(await screen.findByText("200 OK · 142 ms")).toBeInTheDocument();

      typeInto("Name (optional)", "Changed");

      expect(screen.queryByText("200 OK · 142 ms")).not.toBeInTheDocument();
    });

    it("reports a successful test with status and duration", async () => {
      vi.mocked(bridge.webhook_test).mockResolvedValue({ ok: true, statusCode: 200, durationMs: 142, error: null });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Send test" }));

      expect(await screen.findByRole("status")).toHaveTextContent("200 OK · 142 ms");
      expect(bridge.webhook_test).toHaveBeenCalledWith(homeAssistant.id);
    });

    it("ignores a test result after the draft changes", async () => {
      let resolveTest!: (result: { ok: boolean; statusCode: number; durationMs: number; error: null }) => void;
      vi.mocked(bridge.webhook_test).mockReturnValueOnce(new Promise((resolve) => { resolveTest = resolve; }));
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Send test" }));
      typeInto("Name (optional)", "Changed");
      await act(async () => resolveTest({ ok: true, statusCode: 200, durationMs: 142, error: null }));

      await waitFor(() => expect(screen.queryByText("200 OK · 142 ms")).not.toBeInTheDocument());
    });

    it("explains a failed test in plain language", async () => {
      vi.mocked(bridge.webhook_test).mockResolvedValue({ ok: false, statusCode: null, durationMs: 10_000, error: "timeout" });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Send test" }));

      expect(await screen.findByRole("status")).toHaveTextContent("The endpoint didn't answer in time.");
    });

    it("reports the endpoint's status code when it answered with an error", async () => {
      vi.mocked(bridge.webhook_test).mockResolvedValue({ ok: false, statusCode: 503, durationMs: 80, error: "http_503" });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Send test" }));

      expect(await screen.findByRole("status")).toHaveTextContent("The endpoint answered 503.");
    });

    it("shows a friendly message when the test call itself fails", async () => {
      vi.mocked(bridge.webhook_test).mockRejectedValue({ code: "webhook_not_found", message: "gone" });
      renderEditor({ webhook: homeAssistant });

      fireEvent.click(screen.getByRole("button", { name: "Send test" }));

      expect(await screen.findByRole("status")).toHaveTextContent("This webhook no longer exists.");
    });
  });

  it("does not copy a revealed secret after unmount", async () => {
    let resolveReveal!: (value: { id: string; secret: string }) => void;
    vi.mocked(bridge.webhook_reveal_secret).mockReturnValueOnce(new Promise((resolve) => { resolveReveal = resolve; }));
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const { unmount } = renderEditor({ webhook: homeAssistant });

    fireEvent.click(screen.getByRole("button", { name: "Copy secret" }));
    unmount();
    resolveReveal({ id: homeAssistant.id, secret: "whsec_real_secret" });

    await Promise.resolve();
    expect(writeText).not.toHaveBeenCalled();
  });

  describe("queued delivery discard note", () => {
    const discardNote = () => document.getElementById("webhook-queue-discard-note");

    it("warns when a saved webhook change discards queued deliveries", () => {
      renderEditor({ webhook: { ...homeAssistant, pendingDeliveries: 1 } });
      typeInto("Endpoint URL", "https://hooks.example.com/changed");

      expect(discardNote()).toHaveTextContent("Saving this change discards 1 queued delivery.");
      expect(screen.getByRole("button", { name: "Save" })).toHaveAttribute("aria-describedby", "webhook-queue-discard-note");
    });

    it("uses plural copy for several queued deliveries", () => {
      renderEditor({ webhook: { ...homeAssistant, pendingDeliveries: 3 } });
      typeInto("Endpoint URL", "https://hooks.example.com/changed");

      expect(discardNote()).toHaveTextContent("Saving this change discards 3 queued deliveries.");
    });

    it("shows for a mode-only change that keeps the same contacts", () => {
      renderEditor({ webhook: zapier });
      expect(discardNote()).toBeNull();

      fireEvent.click(screen.getByRole("radio", { name: "Everyone except" }));

      expect(discardNote()).toHaveTextContent("Saving this change discards 1 queued delivery.");
    });

    it("shows when switching to Anyone, which drops the filter", () => {
      renderEditor({ webhook: zapier });

      fireEvent.click(screen.getByRole("radio", { name: "Anyone" }));

      expect(discardNote()).toHaveTextContent("Saving this change discards 1 queued delivery.");
    });

    it("does not show for a rename-only change", () => {
      renderEditor({ webhook: zapier });

      typeInto("Name (optional)", "Renamed zapier");

      expect(discardNote()).toBeNull();
      expect(screen.getByRole("button", { name: "Save" })).not.toHaveAttribute("aria-describedby");
    });

    it("places the note after the filter block and before the signing secret", () => {
      renderEditor({ webhook: zapier });
      typeInto("Endpoint URL", "https://hooks.example.com/changed");

      const note = discardNote() as HTMLElement;
      const secret = screen.getByLabelText("Signing secret");
      expect(note.compareDocumentPosition(secret) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(screen.getByLabelText("Contacts or numbers").compareDocumentPosition(note) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });
  });
});
