import { Boxes, ServerCrash } from "lucide-react";
import { MachineCard } from "../components/MachineCard";
import { Button, Callout, Card, CopyButton, EmptyState, InlineCode, PageHeader } from "../components/ui";
import { isWorker } from "../lib/api";
import { engineDownReason, hasCap } from "../lib/caps";
import { usePoll } from "../lib/hooks";
import { useLive } from "../lib/live";
import { platform } from "../lib/platform";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import { PlatformApps } from "./PlatformAppsView";

export function AppsView() {
  const { settings, servers, select } = useSession();
  const { health } = useLive();
  // What the server says now wins: a main server can become a worker after pairing (`vercelify demote`).
  if (isWorker(settings) || health.data?.role === "worker") {
    const mains = servers.filter((s) => !isWorker(s) && s.agent_url !== settings.agent_url);
    return (
      <>
        <PageHeader title="Apps" subtitle={`${settings.hostname} is a worker: it runs apps for its main server.`} />
        <div className="rounded-xl border border-dashed border-line-strong bg-surface/50">
          <EmptyState
            icon={<Boxes />}
            title="Deploy from your main server"
            action={
              mains[0] && (
                <Button variant="primary" onClick={() => void select(mains[0]!.agent_url)}>
                  Switch to {mains[0].hostname}
                </Button>
              )
            }
          >
            Pick this machine under “Run on” when you create an app there, or let the main server place apps on it.
          </EmptyState>
        </div>
      </>
    );
  }
  if (health.data && !hasCap(health.data, "platform")) return <NoEngine hostname={settings.hostname} />;
  return <PlatformApps />;
}

/** No engine here: it didn't start or is turned off, and the agent says why. */
function NoEngine({ hostname }: { hostname: string }) {
  const why = usePoll(() => platform.apps().then(() => undefined, (e) => toCockpitError(e).message), 0);
  if (why.loading) return <PageHeader title="Apps" />;
  const down = engineDownReason(why.data) ?? "the agent didn't say why";
  return (
    <>
      <PageHeader title="Apps" subtitle={`Vercelify's engine isn't running on ${hostname}, so its apps can't be managed from here.`} />
      <div className="grid gap-6 @4xl:grid-cols-[minmax(0,1fr)_300px]">
        <Card title="The engine didn't start" description="Apps that were running keep running; deploys, rollbacks and settings wait for it." icon={<ServerCrash />}>
          <div data-testid="engine-down">
            <Callout tone="bad">{down}</Callout>
          </div>
          <div className="mt-4 flex items-center gap-2 text-[12px] text-fg-muted">
            <span>More detail on the server:</span>
            <InlineCode>sudo vercelify doctor</InlineCode>
            <CopyButton text="sudo vercelify doctor" />
          </div>
        </Card>
        <MachineCard />
      </div>
    </>
  );
}
