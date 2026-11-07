import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { accentAttribute, parseAccentColor } from "./accent-color";
import { persistAccentColor, readAccentColor } from "./layout-persistence";

const LAYOUT_KEY = "peppy.layout.v1";
const PERSISTENCE_ERROR = "Couldn't save the accent color. It will reset when Peppy restarts.";

function accentColorFromLayout(value: string | null): number | null {
  try {
    const layout: unknown = JSON.parse(value ?? "{}");
    return typeof layout === "object" && layout !== null && !Array.isArray(layout)
      ? parseAccentColor((layout as Record<string, unknown>).accentColor)
      : null;
  } catch {
    return null;
  }
}

function readStoredAccentColor(): number | null | undefined {
  try {
    return accentColorFromLayout(localStorage.getItem(LAYOUT_KEY));
  } catch {
    return undefined;
  }
}

export function useAccentColor(): {
  accentColor: number | null;
  selectAccentColor(value: number | null): void;
  error?: string;
} {
  const [accentColor, setAccentColor] = useState(readAccentColor);
  const [error, setError] = useState<string>();
  const observedStoredAccent = useRef(accentColor);

  useEffect(() => {
    const syncAccentColor = (event: StorageEvent) => {
      if (event.key !== LAYOUT_KEY && event.key !== null) return;
      const storedAccent = readStoredAccentColor();
      if (storedAccent === undefined) return;
      if (storedAccent === observedStoredAccent.current) return;
      observedStoredAccent.current = storedAccent;
      setAccentColor(storedAccent);
      setError(undefined);
    };
    window.addEventListener("storage", syncAccentColor);
    return () => window.removeEventListener("storage", syncAccentColor);
  }, []);

  useLayoutEffect(() => {
    const attribute = accentAttribute(accentColor);
    const elements = [document.documentElement, document.body];
    const previousAttributes = elements.map(element => element.getAttribute("data-accent"));
    for (const element of elements) {
      if (attribute === undefined) element.removeAttribute("data-accent");
      else element.setAttribute("data-accent", attribute);
    }
    return () => {
      elements.forEach((element, index) => {
        const previousAttribute = previousAttributes[index];
        if (previousAttribute === null) element.removeAttribute("data-accent");
        else element.setAttribute("data-accent", previousAttribute);
      });
    };
  }, [accentColor]);

  const selectAccentColor = (value: number | null): void => {
    const nextAccentColor = parseAccentColor(value);
    setAccentColor(nextAccentColor);
    if (persistAccentColor(nextAccentColor)) {
      observedStoredAccent.current = nextAccentColor;
      setError(undefined);
    } else {
      setError(PERSISTENCE_ERROR);
    }
  };

  return { accentColor, selectAccentColor, error };
}
