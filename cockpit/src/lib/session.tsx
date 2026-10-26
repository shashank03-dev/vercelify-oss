import { createContext, useContext, type ReactNode } from "react";
import type { CockpitSettings } from "../types";

interface Session {
  /** The selected server. */
  settings: CockpitSettings;
  /** Every paired server (the selected one included). */
  servers: CockpitSettings[];
  /** Forget the selected server (onboarding when it was the last one). */
  unpair: () => Promise<void>;
  /** Save (and select) a server's connection details. */
  update: (s: CockpitSettings) => Promise<void>;
  select: (agentUrl: string) => Promise<void>;
  remove: (agentUrl: string) => Promise<void>;
  /** Open the "add a server" dialog. */
  addServer: () => void;
  /** Bumped when servers are added, removed or pooled: refetch fleet data. */
  revision: number;
}

const Ctx = createContext<Session | null>(null);

export function SessionProvider({ value, children }: { value: Session; children: ReactNode }) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useSession(): Session {
  const s = useContext(Ctx);
  if (!s) throw new Error("useSession outside SessionProvider");
  return s;
}

/** Host part of the agent URL, e.g. "10.77.0.1". */
export function agentHost(s: CockpitSettings): string {
  try {
    return new URL(s.agent_url).hostname;
  } catch {
    return s.hostname;
  }
}
