import { commands, type Theme } from "@/bindings";

/**
 * Appearance theme handling.
 *
 * Voicey ships a single fixed dark theme (settled design: near-black
 * surfaces, hairline borders, one red recording accent). There is no light
 * palette and no theme switcher. The `theme` field is kept in AppSettings for
 * compatibility with the backend's generated bindings, but the UI ignores
 * its value beyond remembering it.
 *
 * The preference is still mirrored to localStorage (under Voicey's own key)
 * so a future theme could be applied synchronously on boot.
 */

export const THEME_STORAGE_KEY = "voicey.theme";

export const THEME_OPTIONS: Theme[] = ["system", "light", "dark"];

const isTheme = (value: unknown): value is Theme =>
  value === "system" || value === "light" || value === "dark";

/** Apply the theme to the document root and remember it for the next launch. */
export const applyTheme = (theme: Theme): void => {
  const root = document.documentElement;
  root.dataset.theme = "dark";
  try {
    localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // localStorage may be unavailable; the setting still persists in
    // AppSettings, so this only costs one harmless skipped write.
  }
};

/** Read the last-stored theme value (Voicey renders dark regardless). */
export const getStoredTheme = (): Theme => {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY);
    if (isTheme(stored)) return stored;
  } catch {
    // ignore
  }
  return "dark";
};

/** Apply the persisted theme from AppSettings (the source of truth). */
export const syncThemeFromSettings = async (): Promise<void> => {
  try {
    const result = await commands.getAppSettings();
    if (result.status === "ok") {
      applyTheme(result.data.theme ?? "dark");
    }
  } catch (e) {
    console.warn("Failed to sync theme from settings:", e);
  }
};
