// Native notifications for server alerts while the app is open (the server
// also reaches your phone through ntfy/Discord with the app closed).
//
// In the desktop app they go through the Tauri notification plugin; in a
// browser (dev mode) through the Notification API. Turned on or off per
// Mac in Settings (kept in localStorage: a viewer preference).

import type { Alert } from "../types";
import { inTauri } from "./transport";

const KEY = "vercelify.notifications";

export function notificationsWanted(): boolean {
  try {
    return localStorage.getItem(KEY) !== "off";
  } catch {
    return true;
  }
}

export function setNotificationsWanted(on: boolean) {
  try {
    localStorage.setItem(KEY, on ? "on" : "off");
  } catch {
    /* private mode: stays on for this session */
  }
}

/** Only problems and recoveries, not routine notices (repairs, cleanups). */
export function shouldNotify(a: Alert): boolean {
  if (/^(repair|cleanup|test):/.test(a.source)) return false;
  return a.resolved || a.status === "bad" || a.status === "warn";
}

export function alertText(hostname: string, a: Alert): { title: string; body: string } {
  return { title: `${hostname}: ${a.title}`, body: a.detail };
}

async function tauriNotify(title: string, body: string) {
  const n = await import("@tauri-apps/plugin-notification");
  let ok = await n.isPermissionGranted();
  if (!ok) ok = (await n.requestPermission()) === "granted";
  if (ok) n.sendNotification({ title, body });
}

export async function notify(title: string, body: string): Promise<void> {
  if (!notificationsWanted()) return;
  try {
    if (inTauri) return await tauriNotify(title, body);
    if (typeof Notification === "undefined") return;
    if (Notification.permission === "default") await Notification.requestPermission();
    if (Notification.permission === "granted") new Notification(title, { body });
  } catch {
    /* notifications are a nicety: never break the app over them */
  }
}
