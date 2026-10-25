import { PauseCircle, PlayCircle, Server, Trash2 } from "lucide-react";
import { useState } from "react";
import { bytes, type Health } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { isPlatform, platformOn } from "../lib/platform";
import { useSession } from "../lib/session";
import type { PlatformNode } from "../types";
import { useToast } from "./Toast";
import { Button, Card, IconButton, Modal, StatusText } from "./ui";

function nodeHealth(n: PlatformNode): { health: Health; label: string } {
  if (!n.online) return { health: "bad", label: "offline" };
  if (n.status.thermal_paused) return { health: "warn", label: "too hot" };
  if (n.role !== "main" && !n.status.docker_ok) return { health: "bad", label: "Docker down" };
  if (n.drained) return { health: "idle", label: "drained" };
  return { health: "ok", label: "online" };
}

/**
 * The servers a platform main server runs apps on (design §8.2): the main
 * server and its workers, with what each reported, drain and remove.
 */
export function PlatformServers() {
  const { servers, revision } = useSession();
  const toast = useToast();
  const main = servers.find(isPlatform);
  const client = main ? platformOn(main.agent_url) : null;
  const nodes = usePoll(() => (client ? client.nodes() : Promise.resolve([])), 30_000, [main?.agent_url, revision]);
  const [removing, setRemoving] = useState<PlatformNode | null>(null);
  const [busy, setBusy] = useState<string>();
  if (!client || !nodes.data?.length) return null;

  async function act(key: string, fn: () => Promise<unknown>, ok: string) {
    setBusy(key);
    try {
      await fn();
      toast.ok(ok);
      await nodes.refresh();
      return true;
    } catch (e) {
      toast.bad((e as Error).message);
      return false;
    } finally {
      setBusy(undefined);
    }
  }

  return (
    <Card
      className="mt-4"
      flush
      testId="platform-servers"
      icon={<Server />}
      title="Servers that run apps"
      description="Apps are placed on the server with the most memory to spare, unless you pin them. Add one: sudo vercelify mesh invite, then join it."
    >
      <div className="divide-y divide-line">
        {nodes.data.map((n) => {
          const h = nodeHealth(n);
          const used = n.status.memory_total_mb ? 1 - n.status.memory_available_mb / n.status.memory_total_mb : 0;
          return (
            <div key={n.id} className="flex flex-wrap items-center gap-x-4 gap-y-1 px-4 py-2.5 text-[13px]" data-testid={`platform-node-${n.name}`}>
              <div className="flex min-w-40 items-center gap-2 font-medium text-fg">
                {n.name}
                <span className="text-[11px] font-normal text-fg-dim">{n.role === "main" ? "main" : "worker"}</span>
              </div>
              <StatusText health={h.health} label={h.label} />
              <span className="font-mono text-[12px] text-fg-muted">{n.mesh_ip || "—"}</span>
              <span className="text-fg-muted">{n.arch}</span>
              {n.status.memory_total_mb > 0 && (
                <span className="text-fg-muted">
                  {bytes(n.status.memory_available_mb * 1_048_576)} free of {bytes(n.status.memory_total_mb * 1_048_576)} ({Math.round(used * 100)}% used)
                </span>
              )}
              <span className="text-fg-muted">{n.instances === 1 ? "1 instance" : `${n.instances} instances`}</span>
              <div className="ml-auto flex items-center gap-1">
                {n.role !== "main" && (
                  <>
                    <Button
                      size="sm"
                      variant="ghost"
                      icon={n.drained ? <PlayCircle /> : <PauseCircle />}
                      busy={busy === `drain-${n.id}`}
                      data-testid={`platform-drain-${n.name}`}
                      onClick={() =>
                        void act(
                          `drain-${n.id}`,
                          () => client.patchNode(n.id, { drained: !n.drained }),
                          n.drained ? `${n.name} takes new apps again` : `${n.name} takes no new apps; what runs there keeps running`,
                        )
                      }
                    >
                      {n.drained ? "Undrain" : "Drain"}
                    </Button>
                    <IconButton label={`Remove ${n.name}`} onClick={() => setRemoving(n)}>
                      <Trash2 />
                    </IconButton>
                  </>
                )}
              </div>
            </div>
          );
        })}
      </div>
      {removing && (
        <Modal open onClose={() => setRemoving(null)} title={`Remove ${removing.name}?`} description="It stops being a place for apps. To take it off the network too, revoke it below.">
          <div className="flex justify-end gap-2" data-testid="platform-remove-confirm">
            <Button variant="ghost" onClick={() => setRemoving(null)}>
              Cancel
            </Button>
            <Button
              variant="danger"
              busy={busy === "remove"}
              data-testid="platform-remove-yes"
              onClick={() => void act("remove", () => client.removeNode(removing.id), `${removing.name} removed`).then((ok) => ok && setRemoving(null))}
            >
              Remove
            </Button>
          </div>
        </Modal>
      )}
    </Card>
  );
}
