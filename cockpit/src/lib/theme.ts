import { useEffect, useState } from "react";

export type ThemePref = "system" | "dark" | "light";

const KEY = "vercelify.theme";
const media = () => window.matchMedia?.("(prefers-color-scheme: light)");

export function loadThemePref(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    return v === "dark" || v === "light" || v === "system" ? v : "dark";
  } catch {
    return "dark";
  }
}

export function applyTheme(pref: ThemePref) {
  const resolved = pref === "system" ? (media()?.matches ? "light" : "dark") : pref;
  document.documentElement.dataset.theme = resolved;
}

/** Current preference; changing it persists, applies and follows the OS when "system". */
export function useTheme(): [ThemePref, (p: ThemePref) => void] {
  const [pref, setPref] = useState<ThemePref>(loadThemePref);
  useEffect(() => {
    applyTheme(pref);
    try {
      localStorage.setItem(KEY, pref);
    } catch {
      /* private mode */
    }
    if (pref !== "system") return;
    const m = media();
    const on = () => applyTheme("system");
    m?.addEventListener("change", on);
    return () => m?.removeEventListener("change", on);
  }, [pref]);
  return [pref, setPref];
}
