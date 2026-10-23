import { BrainCircuit, HardDrive, HeartPulse, Rocket, ScreenShare, Server, Settings, type LucideIcon } from "lucide-react";
import { createContext, useContext } from "react";
import type { ThemePref } from "./theme";

export type Tab = "apps" | "ai" | "servers" | "storage" | "screencast" | "health" | "settings";

export interface NavItem {
  id: Tab;
  label: string;
  /** One line for the command palette and the sidebar tooltip. */
  hint: string;
  icon: LucideIcon;
  /** Sidebar section; "footer" sits at the bottom with no heading. */
  group: "Projects" | "Machines" | "footer";
}

/** Single source for the sidebar, keyboard shortcuts (⌘1…) and the command palette. */
export const NAV: NavItem[] = [
  { id: "apps", label: "Apps", hint: "Deploy from Git, databases, domains and logs", icon: Rocket, group: "Projects" },
  { id: "ai", label: "AI models", hint: "Run open-source AI models on your machines", icon: BrainCircuit, group: "Projects" },
  { id: "servers", label: "Servers", hint: "Every machine you've connected, side by side", icon: Server, group: "Machines" },
  { id: "health", label: "Health", hint: "CPU, memory, temperature, battery and history", icon: HeartPulse, group: "Machines" },
  { id: "storage", label: "Storage", hint: "Disks and where app data is kept", icon: HardDrive, group: "Machines" },
  { id: "screencast", label: "Second screen", hint: "Use the laptop's screen as a monitor for this Mac", icon: ScreenShare, group: "Machines" },
  { id: "settings", label: "Settings", hint: "Alerts, backups, access, connection and appearance", icon: Settings, group: "footer" },
];

export const NAV_GROUPS = ["Projects", "Machines"] as const;

export const isTab = (v: string): v is Tab => NAV.some((n) => n.id === v);

export interface Nav {
  tab: Tab;
  go: (tab: Tab, intent?: { app?: string; newApp?: boolean }) => void;
  /** Something the user asked for from the palette; consumed by the page. */
  intent: { app?: string; newApp?: boolean } | null;
  clearIntent: () => void;
  openPalette: () => void;
  theme: ThemePref;
  setTheme: (t: ThemePref) => void;
}

const Ctx = createContext<Nav | null>(null);
export const NavProvider = Ctx.Provider;

export function useNav(): Nav {
  const v = useContext(Ctx);
  if (!v) throw new Error("useNav outside NavProvider");
  return v;
}
