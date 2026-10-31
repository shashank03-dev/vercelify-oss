import { usePoll } from "../lib/hooks";
import type { PlatformClient } from "../lib/platform";
import { StatusText } from "./ui";

export function DatabaseStatus({ id, client }: { id: string; client: PlatformClient }) {
  const runtime = usePoll(() => client.databaseStatus(id), 8000, [id, client]);
  const state = runtime.data?.state;
  const operation = runtime.data?.operation;
  const starting = operation?.status === "running";
  const failed = operation?.status === "failed" && state !== "running";
  const label = runtime.error ? "Unknown" : !state ? "Checking" : starting ? "Starting" : state === "running" ? "Running" : failed ? "Failed" : state === "paused" ? "Paused" : "Stopped";
  return <span title={operation?.detail ?? runtime.error?.message}><StatusText health={label === "Running" ? "ok" : failed ? "bad" : starting ? "warn" : "idle"} label={label} /></span>;
}
