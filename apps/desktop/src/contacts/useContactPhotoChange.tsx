import { useEffect, useRef, useState } from "react";
import type { ReactElement } from "react";
import { Modal } from "@peppy/desktop-ui";
import { bridge } from "../bridge/bridge";

type PickedPhoto = { dataUrl: string; naturalWidth: number; naturalHeight: number };

/**
 * Picks a contact photo and walks through the square crop. `onCropped` receives the
 * cropped JPEG data URL, which is the value stored as a `photo: { kind: "set" }` edit.
 * Render `cropDialog` where the dialog should mount; it is null until a photo is picked.
 */
export function useContactPhotoChange({ onCropped }: { onCropped(croppedDataUrl: string): void }): {
  choosePhoto(opener: HTMLElement | null): Promise<void>;
  picking: boolean;
  error?: string;
  cropDialog: ReactElement | null;
} {
  const [picking, setPicking] = useState(false);
  const [error, setError] = useState<string>();
  const [photo, setPhoto] = useState<PickedPhoto | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);

  const choosePhoto = async (opener: HTMLElement | null) => {
    openerRef.current = opener;
    setError(undefined);
    setPicking(true);
    try {
      const picked = await bridge.pick_contact_photo();
      if (picked) setPhoto(picked);
    } catch (e) {
      setError(String((e as { message?: string })?.message ?? e));
    } finally {
      setPicking(false);
    }
  };

  const handleConfirm = (croppedDataUrl: string) => {
    setPhoto(null);
    onCropped(croppedDataUrl);
  };

  const cropDialog = photo
    ? (
      <PhotoCropDialog
        photo={photo}
        opener={openerRef}
        onConfirm={handleConfirm}
        onCancel={() => setPhoto(null)}
      />
    )
    : null;

  return { choosePhoto, picking, error, cropDialog };
}

export function PhotoCropDialog({
  photo,
  opener,
  onConfirm,
  onCancel,
}: {
  photo: { dataUrl: string; naturalWidth: number; naturalHeight: number };
  opener: React.RefObject<HTMLElement | null>;
  onConfirm(croppedDataUrl: string): void;
  onCancel(): void;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [cropState, setCropState] = useState<"loading" | "ready" | "error">("loading");
  const [isDragging, setIsDragging] = useState(false);
  const dragStart = useRef({ x: 0, y: 0 });

  const targetSize = 256;
  const minDim = Math.min(photo.naturalWidth, photo.naturalHeight);
  const scale = targetSize / minDim;
  const cropSize = minDim;

  const handleMouseDown = (e: React.MouseEvent) => {
    setIsDragging(true);
    dragStart.current = { x: e.clientX - offset.x, y: e.clientY - offset.y };
  };

  const handleMouseMove = (e: React.MouseEvent) => {
    if (!isDragging) return;
    const newX = e.clientX - dragStart.current.x;
    const newY = e.clientY - dragStart.current.y;
    const maxX = Math.max(0, (photo.naturalWidth - cropSize) * scale);
    const maxY = Math.max(0, (photo.naturalHeight - cropSize) * scale);
    setCropState("loading");
    setOffset({
      x: Math.max(0, Math.min(newX, maxX)),
      y: Math.max(0, Math.min(newY, maxY)),
    });
  };

  const handleMouseUp = () => {
    setIsDragging(false);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    const step = e.shiftKey ? 32 : 8;
    const maxX = Math.max(0, (photo.naturalWidth - cropSize) * scale);
    const maxY = Math.max(0, (photo.naturalHeight - cropSize) * scale);

    if (e.key === "ArrowUp") {
      e.preventDefault();
      setCropState("loading");
      setOffset(prev => ({
        ...prev,
        y: Math.max(0, prev.y - step),
      }));
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setCropState("loading");
      setOffset(prev => ({
        ...prev,
        y: Math.min(maxY, prev.y + step),
      }));
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      setCropState("loading");
      setOffset(prev => ({
        ...prev,
        x: Math.max(0, prev.x - step),
      }));
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      setCropState("loading");
      setOffset(prev => ({
        ...prev,
        x: Math.min(maxX, prev.x + step),
      }));
    }
  };

  useEffect(() => {
    let current = true;
    setCropState("loading");
    const img = new Image();
    img.onload = () => {
      const canvas = canvasRef.current;
      if (!canvas || !current) return;
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        setCropState("error");
        return;
      }
      try {
        canvas.width = targetSize;
        canvas.height = targetSize;
        ctx.fillStyle = "#fff";
        ctx.fillRect(0, 0, targetSize, targetSize);
        ctx.drawImage(
          img,
          offset.x / scale,
          offset.y / scale,
          cropSize,
          cropSize,
          0,
          0,
          targetSize,
          targetSize
        );
        if (current) setCropState("ready");
      } catch {
        if (current) setCropState("error");
      }
    };
    img.onerror = () => { if (current) setCropState("error"); };
    img.src = photo.dataUrl;
    return () => { current = false; };
  }, [photo.dataUrl, offset, scale, cropSize, targetSize]);

  const handleConfirm = () => {
    if (cropState !== "ready") return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    try {
      const jpeg = canvas.toDataURL("image/jpeg", 0.85);
      onConfirm(jpeg);
    } catch {
      setCropState("error");
    }
  };

  return (
    <Modal
      id="photo-crop-dialog"
      className="photo-crop-panel"
      title="Crop photo to square"
      opener={opener}
      onDismiss={onCancel}
      actions={
        <>
          <button type="button" className="secondary-button" onClick={onCancel}>
            Cancel
          </button>
          <button type="button" className="primary-button" onClick={handleConfirm} disabled={cropState !== "ready"}>
            Confirm crop
          </button>
        </>
      }
    >
      <div
        id="photo-crop-viewport"
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
        onKeyDown={handleKeyDown}
        tabIndex={0}
        aria-label="Photo crop position"
        aria-describedby="photo-crop-hint"
      >
        <img
          id="photo-crop-image"
          src={photo.dataUrl}
          alt="Original photo to crop"
          style={{
            background: "#fff",
            width: photo.naturalWidth * scale,
            height: photo.naturalHeight * scale,
            transform: `translate(-${offset.x}px, -${offset.y}px)`,
          }}
        />
      </div>
      <canvas id="photo-crop-preview" ref={canvasRef} width={targetSize} height={targetSize} />
      {cropState === "loading" && <p id="photo-crop-status" role="status">Loading photo…</p>}
      {cropState === "error" && <p id="photo-crop-status" role="alert">This photo couldn’t be loaded. Cancel and choose another photo.</p>}
      <p id="photo-crop-hint">Use arrow keys or drag to position. Shift+arrow moves faster.</p>
    </Modal>
  );
}
