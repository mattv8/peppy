import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bridge } from "../bridge/bridge";
import { useContactPhotoChange } from "./useContactPhotoChange";

vi.mock("../bridge/bridge", { spy: true });

const picked = { dataUrl: "data:image/png;base64,transparent", naturalWidth: 256, naturalHeight: 256 };

class DeferredImage {
  static instances: DeferredImage[] = [];
  public onload: (() => void) | null = null;
  public onerror: (() => void) | null = null;
  constructor() { DeferredImage.instances.push(this); }
  public set src(_value: string) { /* loaded by the test through onload */ }
}

function Harness({ onCropped }: { onCropped(croppedDataUrl: string): void }) {
  const { choosePhoto, picking, error, cropDialog } = useContactPhotoChange({ onCropped });
  return (
    <>
      <button type="button" id="pick-photo" onClick={event => choosePhoto(event.currentTarget)}>Pick photo</button>
      {picking && <p data-testid="picking">Picking</p>}
      {error && <p role="alert">{error}</p>}
      {cropDialog}
    </>
  );
}

describe("useContactPhotoChange", () => {
  beforeEach(() => {
    DeferredImage.instances = [];
    vi.stubGlobal("Image", DeferredImage);
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ fillStyle: "", fillRect: vi.fn(), drawImage: vi.fn() } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue("data:image/jpeg;base64,cropped");
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    vi.mocked(bridge.pick_contact_photo).mockReset();
  });

  it("opens no crop dialog and shows no error when the picker is cancelled", async () => {
    vi.mocked(bridge.pick_contact_photo).mockResolvedValue(null as unknown as Awaited<ReturnType<typeof bridge.pick_contact_photo>>);
    render(<Harness onCropped={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Pick photo" }));

    await waitFor(() => expect(bridge.pick_contact_photo).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("status")).toBeNull();
    expect(document.getElementById("photo-crop-dialog")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("surfaces the picker error message the way ContactForm did", async () => {
    vi.mocked(bridge.pick_contact_photo).mockRejectedValue(new Error("Photo library unavailable"));
    render(<Harness onCropped={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Pick photo" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Photo library unavailable");
    expect(document.getElementById("photo-crop-dialog")).toBeNull();
  });

  it("reports the picking state while the picker is open", async () => {
    let resolvePick!: (value: typeof picked) => void;
    vi.mocked(bridge.pick_contact_photo).mockImplementation(() => new Promise(resolve => { resolvePick = resolve; }));
    render(<Harness onCropped={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Pick photo" }));
    expect(await screen.findByTestId("picking")).toHaveTextContent("Picking");

    await act(async () => resolvePick(picked));
    expect(screen.queryByTestId("picking")).toBeNull();
  });

  it("shows the crop dialog for a picked photo and reports the cropped photo once on confirm", async () => {
    vi.mocked(bridge.pick_contact_photo).mockResolvedValue(picked);
    const onCropped = vi.fn();
    render(<Harness onCropped={onCropped} />);

    fireEvent.click(screen.getByRole("button", { name: "Pick photo" }));
    expect(await screen.findByRole("dialog", { name: "Crop photo to square" })).toBeInTheDocument();

    act(() => DeferredImage.instances[0]?.onload?.());
    fireEvent.click(screen.getByRole("button", { name: "Confirm crop" }));

    expect(onCropped).toHaveBeenCalledTimes(1);
    expect(onCropped).toHaveBeenCalledWith("data:image/jpeg;base64,cropped");
    await waitFor(() => expect(document.getElementById("photo-crop-dialog")).toBeNull());
  });

  it("closes the crop dialog without reporting a photo when cancelled, and returns focus to the opener", async () => {
    vi.mocked(bridge.pick_contact_photo).mockResolvedValue(picked);
    const onCropped = vi.fn();
    render(<Harness onCropped={onCropped} />);
    const opener = screen.getByRole("button", { name: "Pick photo" });

    fireEvent.click(opener);
    await screen.findByRole("dialog", { name: "Crop photo to square" });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(document.getElementById("photo-crop-dialog")).toBeNull());
    expect(onCropped).not.toHaveBeenCalled();
    expect(opener).toHaveFocus();
  });
});
