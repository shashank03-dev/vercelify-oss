import { createContext, useContext, type ReactNode } from "react";
import { useToast } from "../components/Toast";
import { mesh } from "./api";
import { browserUrl } from "./browser-url";
import { usePoll } from "./hooks";
import { openExternal } from "./open";

const Userspace = createContext(false);

export function BrowserProvider({ children }: { children: ReactNode }) {
  const local = usePoll(() => mesh.local(), 30_000);
  return <Userspace.Provider value={local.data?.userspace ?? false}>{children}</Userspace.Provider>;
}

export function useBrowser() {
  const userspace = useContext(Userspace);
  const toast = useToast();
  const url = (value: string) => browserUrl(value, userspace);
  const open = (value: string) => openExternal(url(value)).catch((e: unknown) => toast.bad(`Could not open browser: ${String(e)}`));
  return { url, open };
}
