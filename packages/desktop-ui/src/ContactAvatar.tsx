import { useState } from "react";
import { User, Users } from "lucide-react";
import { initials, paletteIndex, type AvatarKind } from "./avatar";

type ContactAvatarProps = {
  seed: string;
  kind?: AvatarKind;
  photoDataUrl?: string;
  name?: string;
  size?: 18 | 36 | 64;
  id?: string;
  className?: string;
  "data-contact-id"?: string;
};

const GLYPH_SIZES = {
  18: { initials: 8, icon: 10, strokeWidth: 1.75 },
  36: { initials: 14, icon: 18, strokeWidth: 1.75 },
  64: { initials: 26, icon: 32, strokeWidth: 1.5 },
} as const;

/** Decorative contact portrait with deterministic color and safe fallbacks. */
export function ContactAvatar({
  seed,
  kind = "person",
  photoDataUrl,
  name,
  size = 36,
  id,
  className,
  "data-contact-id": contactId,
}: ContactAvatarProps) {
  const [failedUrl, setFailedUrl] = useState<string | undefined>(undefined);
  const glyphSize = GLYPH_SIZES[size];
  const avatarInitials = initials(name, size >= 36 ? 2 : 1);
  const colorIndex = paletteIndex(seed);
  const photoFailed = photoDataUrl !== undefined && failedUrl === photoDataUrl;

  const avatarKind = kind === "group"
    ? "group"
    : photoDataUrl && !photoFailed
      ? "photo"
      : avatarInitials
        ? "initials"
        : "person";

  return (
    <span
      id={id}
      className={`contact-avatar${className ? ` ${className}` : ""}`}
      aria-hidden="true"
      data-avatar-kind={avatarKind}
      data-avatar-color={colorIndex}
      data-contact-id={contactId}
      style={{
        width: size,
        height: size,
        flex: `0 0 ${size}px`,
        background: `var(--peppy-avatar-${colorIndex}, var(--peppy-accent))`,
        color: "var(--peppy-avatar-glyph, var(--peppy-accent-text))",
      }}
    >
      {avatarKind === "group" ? (
        <Users size={glyphSize.icon} strokeWidth={glyphSize.strokeWidth} aria-hidden />
      ) : avatarKind === "photo" ? (
        <img src={photoDataUrl} alt="" onError={() => setFailedUrl(photoDataUrl)} />
      ) : avatarKind === "initials" ? (
        <span style={{ fontSize: glyphSize.initials }}>{avatarInitials}</span>
      ) : (
        <User size={glyphSize.icon} strokeWidth={glyphSize.strokeWidth} aria-hidden />
      )}
    </span>
  );
}

export type { ContactAvatarProps };
