import { Network, ShieldOff, UserPlus } from "lucide-react";
import { useState } from "react";
import { agentOn, mesh } from "../lib/api";
import { bytes, type Health } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import type { MeshInvite, MeshLocal, MeshNode, MeshPeer } from "../types";
import { useToast } from "./Toast";
import { Button, Callout, Card, CodeBlock, ErrorNote, InlineCode, Modal, Segmented, StatusDot, StatusText } from "./ui";

const PATH_LABEL: Record<string, string> = { lan: "LAN", direct: "direct", relay: "relay", none: "no path" };
const pathHealth = (p?: string): Health => (p === "lan" || p === "direct" ? "ok" : p === "relay" ? "warn" : "idle");

function ago(iso: string | null | undefined): string {
  if (!iso) return "never";
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 90) return "just now";
  if (s < 5400) return `${Math.round(s / 60)} min ago`;
  if (s < 172_800) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86_400)} days ago`;
}

/** The paired server that coordinates the mesh (answers /v1/mesh/nodes). */
async function findCoordinator(urls: string[]): Promise<{ url: string; nodes: MeshNode[] } | null> {
  for (const url of urls) {
    try {
      return { url, nodes: await agentOn(url).meshNodes() };
    } catch {
      /* not a coordinator, or an older agent */
    }
  }
  return null;
}

/** How this computer reaches a node right now. */
function PathCell({ peer }: { peer?: MeshPeer }) {
  if (!peer) return <span className="text-fg-dim">—</span>;
  const label = PATH_LABEL[peer.path] ?? peer.path;
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
      <StatusDot health={pathHealth(peer.path)} />
      {label}
      {peer.rtt_ms != null && <span className="font-mono text-[11px] text-fg-dim">{peer.rtt_ms.toFixed(1)} ms</span>}
    </span>
  );
}

/** The daemon stopped (a crash, a launchd hiccup): start it from here. */
function StartNetwork() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  return (
    <span className="inline-flex items-center gap-2">
      <Button
        size="sm"
        variant="subtle"
        data-testid="mesh-start"
        busy={busy}
        onClick={async () => {
          setBusy(true);
          setError(undefined);
          try {
            await mesh.start();
          } catch (e) {
            setError(toCockpitError(e).message);
          } finally {
            setBusy(false);
          }
        }}
      >
        Start network
      </Button>
      {error && <span className="text-bad">{error}</span>}
    </span>
  );
}

export function LocalLine({ local }: { local?: MeshLocal }) {
  if (!local) return null;
  if (!local.installed)
    return (
      <p className="text-xs text-fg-muted">
        This computer isn't on Vercelify's network yet: run <InlineCode>install-mac.sh</InlineCode> again (it adds{" "}
        <InlineCode>vercelify-mesh</InlineCode>), then pair a server with its code.
      </p>
    );
  if (local.revoked) return <StatusText health="bad" label="This computer was taken off the network — pair again to rejoin" testId="mesh-local" />;
  if (!local.name) return <StatusText health="idle" label="vercelify-mesh is installed; pair a server to join its network" testId="mesh-local" />;
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-fg-muted" data-testid="mesh-local">
      <StatusText health={local.running ? "ok" : "bad"} label={local.running ? `This computer: ${local.name}` : `${local.name}: vercelify-mesh isn't running`} />
      {!local.running && <StartNetwork />}
      <span className="font-mono">{local.ipv4}</span>
      <span>{local.userspace ? "app mode (no admin rights needed)" : "system mode"}</span>
      {local.coordinator_seen_secs != null && local.coordinator_seen_secs > 120 && (
        <span className="text-warn">coordinator last heard {Math.round(local.coordinator_seen_secs / 60)} min ago</span>
      )}
    </div>
  );
}

function InviteModal({ coordinator, onClose }: { coordinator: string; onClose: () => void }) {
  const [kind, setKind] = useState<"server" | "device">("server");
  const [invite, setInvite] = useState<MeshInvite>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ReturnType<typeof toCockpitError>>();
  async function make() {
    setBusy(true);
    setError(undefined);
    try {
      setInvite(await agentOn(coordinator).meshInvite(kind));
    } catch (e) {
      setError(toCockpitError(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal open onClose={onClose} title="Add a machine to the network" description="A single-use token, valid for an hour.">
      <div className="space-y-3" data-testid="mesh-invite">
        <Segmented
          label="What to add"
          value={kind}
          onChange={(k) => (setKind(k), setInvite(undefined))}
          options={[
            { value: "server", label: "A server" },
            { value: "device", label: "Another computer" },
          ]}
        />
        {!invite ? (
          <Button variant="primary" icon={<UserPlus />} busy={busy} onClick={() => void make()} data-testid="mesh-invite-create">
            Create the token
          </Button>
        ) : kind === "server" ? (
          <>
            <p className="text-[13px] text-fg-muted">On the new server (after the installer), run:</p>
            <CodeBlock code={`sudo vercelify mesh join ${invite.token}`} prompt testId="mesh-invite-command" />
          </>
        ) : (
          <>
            <p className="text-[13px] text-fg-muted">On the other computer, with vercelify-mesh installed:</p>
            <CodeBlock code={`vercelify-mesh join ${invite.token} --port 0`} prompt testId="mesh-invite-command" />
          </>
        )}
        <ErrorNote error={error} />
      </div>
    </Modal>
  );
}

/**
 * Vercelify's own network (design §13): this computer's link to each server
 * (LAN, direct through NAT, or the relay) and, from the coordinator, every
 * node, with revoke.
 */
export function MeshNetwork() {
  const { servers, revision } = useSession();
  const toast = useToast();
  const local = usePoll(() => mesh.local(), 10_000);
  const coord = usePoll(() => findCoordinator(servers.map((s) => s.agent_url)), 30_000, [revision, servers.length]);
  const [revoke, setRevoke] = useState<MeshNode | null>(null);
  const [revoking, setRevoking] = useState(false);
  const [inviting, setInviting] = useState(false);
  const c = coord.data;
  const peers = local.data?.peers ?? [];
  // Nothing to show: no mesh here and no coordinator among the servers.
  if (!c && !local.data?.name && !local.data?.installed) return null;

  async function doRevoke(n: MeshNode) {
    if (!c) return;
    setRevoking(true);
    try {
      await agentOn(c.url).meshRevoke(n.id);
      toast.ok(`${n.name} is off the network; its peers drop it within seconds`);
      setRevoke(null);
      await coord.refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setRevoking(false);
    }
  }

  const rows = c?.nodes ?? [];
  return (
    <Card
      className="mt-4"
      flush
      testId="mesh-network"
      icon={<Network />}
      title="Vercelify network"
      description="Your servers and devices on Vercelify's own encrypted WireGuard network."
      actions={
        c && (
          <Button size="sm" icon={<UserPlus />} onClick={() => setInviting(true)} data-testid="mesh-invite-open">
            Add a machine
          </Button>
        )
      }
    >
      <div className="border-b border-line px-4 py-2.5">
        <LocalLine local={local.data} />
      </div>
      {rows.length > 0 ? (
        <div className="overflow-x-auto">
          <table className="w-full text-[13px]">
            <thead>
              <tr className="text-left text-[11px] uppercase tracking-wide text-fg-dim">
                <th className="px-4 py-2 font-medium">Node</th>
                <th className="px-2 py-2 font-medium">Address</th>
                <th className="px-2 py-2 font-medium">From this computer</th>
                <th className="px-2 py-2 font-medium">Last seen</th>
                <th className="px-4 py-2" />
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {rows.map((n) => {
                const peer = peers.find((p) => p.name === n.name);
                const me = local.data?.name === n.name;
                return (
                  <tr key={n.id} data-testid={`mesh-node-${n.name}`} className={n.revoked_at ? "text-fg-dim" : undefined}>
                    <td className="px-4 py-2">
                      <div className="flex items-center gap-2 font-medium">
                        {n.name}
                        <span className="text-[11px] font-normal text-fg-dim">
                          {n.coordinator ? "coordinator" : n.kind}
                          {n.kind === "device" && n.role !== "owner" ? ` · ${n.role}` : ""}
                        </span>
                      </div>
                      {n.relay_connected && n.relayed_bytes > 0 && (
                        <div className="text-[11px] text-fg-dim">on the relay · {bytes(n.relayed_bytes)} relayed</div>
                      )}
                    </td>
                    <td className="px-2 py-2 font-mono text-[12px] text-fg-muted">{n.ipv4}</td>
                    <td className="px-2 py-2">
                      {n.revoked_at ? (
                        <StatusText health="bad" label="revoked" />
                      ) : me ? (
                        <span className="text-fg-dim">this computer</span>
                      ) : (
                        <PathCell peer={peer} />
                      )}
                    </td>
                    <td className="px-2 py-2 text-fg-muted">{ago(n.last_seen_at)}</td>
                    <td className="px-4 py-2 text-right">
                      {!n.coordinator && !n.revoked_at && (
                        <Button size="sm" variant="ghost" icon={<ShieldOff />} onClick={() => setRevoke(n)} data-testid={`mesh-revoke-${n.name}`}>
                          Revoke
                        </Button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      ) : peers.length > 0 ? (
        <ul className="divide-y divide-line">
          {peers.map((p) => (
            <li key={p.name} className="flex items-center justify-between px-4 py-2 text-[13px]" data-testid={`mesh-peer-${p.name}`}>
              <span className="font-medium">{p.name}</span>
              <PathCell peer={p} />
            </li>
          ))}
        </ul>
      ) : (
        <div className="px-4 py-3">
          <Callout tone="idle">
            Make a server the network's coordinator with <InlineCode>sudo vercelify mesh init</InlineCode>, then pair this computer with the
            code from <InlineCode>sudo vercelify pair</InlineCode>.
          </Callout>
        </div>
      )}

      {revoke && (
        <Modal
          open
          onClose={() => setRevoke(null)}
          title={`Revoke ${revoke.name}?`}
          description="It loses its place on the network at once. To bring it back, invite it again."
        >
          <div className="flex justify-end gap-2" data-testid="mesh-revoke-confirm">
            <Button variant="ghost" onClick={() => setRevoke(null)}>
              Cancel
            </Button>
            <Button variant="danger" busy={revoking} onClick={() => void doRevoke(revoke)} data-testid="mesh-revoke-yes">
              Revoke
            </Button>
          </div>
        </Modal>
      )}
      {inviting && c && <InviteModal coordinator={c.url} onClose={() => setInviting(false)} />}
    </Card>
  );
}
