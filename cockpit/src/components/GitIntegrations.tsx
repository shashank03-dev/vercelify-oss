import { ExternalLink, GitBranch, Webhook } from "lucide-react";
import { useState } from "react";
import { openExternal } from "../lib/open";
import type { PlatformClient } from "../lib/platform";
import type { GithubAppStart, GitSource, WebhookSetup } from "../types";
import { useToast } from "./Toast";
import { Button, Callout, CopyButton, Field, InlineCode, Modal, TextInput } from "./ui";

/** `…/callback?code=…&state=…` (what GitHub sent the browser to) → its parts. */
export function parseCallback(address: string): { code: string; state: string } | null {
  try {
    const u = new URL(address.trim());
    const code = u.searchParams.get("code");
    const state = u.searchParams.get("state");
    return code && state ? { code, state } : null;
  } catch {
    return null;
  }
}

/**
 * Create a GitHub App for this server (design §11): GitHub's manifest flow,
 * then the app shows up as a git source. Its installation tokens replace
 * personal tokens; its webhook makes pushes deploy right away.
 */
export function GithubAppModal({ client, onClose, onCreated }: { client: PlatformClient; onClose: () => void; onCreated: (g: GitSource) => void }) {
  const toast = useToast();
  const [org, setOrg] = useState("");
  const [start, setStart] = useState<GithubAppStart>();
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState(false);
  const parsed = parseCallback(address);

  async function begin() {
    setBusy(true);
    try {
      const s = await client.startGithubApp(org.trim() || undefined);
      setStart(s);
      await openExternal(s.start_url);
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function finish() {
    if (!parsed) return;
    setBusy(true);
    try {
      const g = await client.convertGithubApp(parsed.code, parsed.state);
      toast.ok(`${g.name} is ready: install it on your repositories`);
      onCreated(g);
      if (g.app_url) void openExternal(`${g.app_url}/installations/new`).catch((e: unknown) => toast.bad(`Could not open browser: ${String(e)}`));
      onClose();
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open onClose={onClose} title="Connect a GitHub App" description="Private repositories without personal tokens, and deploys the moment you push.">
      <div className="space-y-3" data-testid="github-app">
        {!start ? (
          <>
            <Field label="Organization (optional)" hint="Leave empty to create it on your own account">
              <TextInput value={org} onChange={(e) => setOrg(e.target.value)} placeholder="acme" spellCheck={false} data-testid="github-app-org" />
            </Field>
            <Button variant="primary" icon={<GitBranch />} busy={busy} onClick={() => void begin()} data-testid="github-app-start">
              Create it on GitHub
            </Button>
          </>
        ) : (
          <>
            <p className="text-[13px] text-fg-muted">
              GitHub opened in your browser. Confirm there; it comes back to this server by itself and the app appears under Credentials.
            </p>
            <Button size="sm" variant="ghost" icon={<ExternalLink />} onClick={() => void openExternal(start.start_url).catch((e: unknown) => toast.bad(`Could not open browser: ${String(e)}`))}>
              Open GitHub again
            </Button>
            <Field label="Didn't come back?" hint="Paste the address your browser ended on (it has ?code=…)">
              <TextInput value={address} onChange={(e) => setAddress(e.target.value)} placeholder="http://…/v1/github-app/callback?code=…" spellCheck={false} data-testid="github-app-address" />
            </Field>
            <Button disabled={!parsed} busy={busy} onClick={() => void finish()} data-testid="github-app-finish">
              Finish
            </Button>
          </>
        )}
      </div>
    </Modal>
  );
}

/** Turn on a webhook for a git source: where to send events and the secret. */
export function WebhookModal({ client, source, onClose }: { client: PlatformClient; source: GitSource; onClose: () => void }) {
  const toast = useToast();
  const [setup, setSetup] = useState<WebhookSetup>();
  const [busy, setBusy] = useState(false);
  async function make() {
    setBusy(true);
    try {
      setSetup(await client.webhook(source.id));
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal open onClose={onClose} title={`Webhook for ${source.name}`} description="Pushes and pull requests deploy at once instead of within a minute.">
      <div className="space-y-3 text-[13px]" data-testid="webhook-setup">
        {!setup ? (
          <>
            {source.webhook && <Callout tone="idle">It has a webhook already: a new secret replaces the old one.</Callout>}
            <Button variant="primary" icon={<Webhook />} busy={busy} onClick={() => void make()} data-testid="webhook-create">
              {source.webhook ? "New secret" : "Set it up"}
            </Button>
          </>
        ) : (
          <>
            {setup.url ? (
              <Field label="Payload URL">
                <div className="flex items-center gap-2">
                  <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-bg px-2 py-1.5 font-mono text-[12px]" data-testid="webhook-url">
                    {setup.url}
                  </code>
                  <CopyButton text={setup.url} />
                </div>
              </Field>
            ) : (
              <Callout tone="warn">
                This server has no public address, so GitHub can't reach it (the poller still deploys within a minute). A provider on your network can use{" "}
                <InlineCode>{setup.lan_urls[0] ?? "the LAN address"}</InlineCode>.
              </Callout>
            )}
            <Field label="Secret" hint="Shown once: paste it into the provider's webhook settings">
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-bg px-2 py-1.5 font-mono text-[12px]" data-testid="webhook-secret">
                  {setup.secret}
                </code>
                <CopyButton text={setup.secret} />
              </div>
            </Field>
            <p className="text-xs text-fg-muted">
              Content type <InlineCode>{setup.content_type}</InlineCode>; events: {setup.events.join(", ")}.
            </p>
          </>
        )}
      </div>
    </Modal>
  );
}
