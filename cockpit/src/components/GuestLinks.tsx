import { useState } from "react";
import { useLive } from "../lib/live";
import { hasCap } from "../lib/caps";
import { usePoll } from "../lib/hooks";
import type { PlatformClient, GuestInvitation } from "../lib/platform";
import { Button, Callout, Card, CopyButton, ErrorNote, Field, Modal, TextInput, Toggle } from "./ui";

export function GuestLinksButton({ app, pr, url, privatePreview, client }: { app: string; pr: number; url: string; privatePreview: boolean; client: PlatformClient }) {
  const [open, setOpen] = useState(false);
  const { health } = useLive();
  const supported = hasCap(health.data, "preview-guest-links");
  return <><Button size="sm" onClick={() => setOpen(true)} data-testid={`share-preview-${pr}`}>Share</Button>
    {open && <GuestLinksDialog app={app} pr={pr} url={url} privatePreview={privatePreview} supported={supported} client={client} onClose={() => setOpen(false)} />}
  </>;
}
function GuestLinksDialog({ app, pr, url, privatePreview, supported, client, onClose }: { app: string; pr: number; url: string; privatePreview: boolean; supported: boolean; client: PlatformClient; onClose: () => void }) {
  const settings = usePoll(() => supported ? client.guestSettings() : Promise.resolve(null), supported ? 15_000 : 0, [supported, client]);
  const grants = usePoll(() => supported ? client.guestInvitations(app, pr) : Promise.resolve([]), supported ? 10_000 : 0, [supported, app, pr, client]);
  const [created, setCreated] = useState<GuestInvitation>();
  const [singleUse, setSingleUse] = useState(true);
  const [ttl, setTtl] = useState(3600);
  const [domain, setDomain] = useState<string>();
  const [port, setPort] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<Error>();
  async function act(work: () => Promise<unknown>) {
    setError(undefined); setBusy(true);
    try { await work(); await grants.refresh(); await settings.refresh(); } catch (e) { setError(e as Error); } finally { setBusy(false); }
  }
  const configured = !!settings.data?.config.domain;
  const editDomain = domain ?? settings.data?.config.domain ?? "";
  const editPort = port ?? String(settings.data?.config.public_port ?? 443);
  return <Modal open onClose={onClose} title={`Share preview #${pr}`} description="Choose who can open this preview">
    <div className="space-y-4 overflow-y-auto p-5" data-testid="guest-links-dialog">
      <Card title={privatePreview ? "Private link" : "Preview link"} description={privatePreview ? "The recipient needs your Vercelify network." : "This preview is public."}>
        <div className="flex min-w-0 items-center gap-3"><code className="min-w-0 flex-1 break-all text-xs">{url}</code><CopyButton text={url} label="Copy preview link" /></div>
      </Card>
      {!supported ? <Callout tone="idle">Update this server to create expiring guest invitations.</Callout> : !privatePreview ? <Callout tone="idle">Use Private previews to issue restricted guest invitations.</Callout> : <Card title="Guest invitation" description="An owner can invite someone outside the Vercelify network.">
        <div className="space-y-4">
          {!configured && <Callout tone="idle">No public guest domain is configured. The private link above still needs the Vercelify network.</Callout>}
          {configured && <p className="text-xs text-fg-muted">Guest domain: {settings.data?.config.domain}. Its DNS and HTTPS certificate must already work; availability has not been verified.</p>}
          <Field label="Invitation expires after"><select aria-label="Invitation expires after" value={ttl} onChange={e => setTtl(Number(e.target.value))} className="w-full rounded border border-line bg-bg p-2 text-sm">
            <option value={900}>15 minutes</option><option value={3600}>1 hour</option><option value={86400}>1 day</option><option value={604800}>7 days</option>
          </select></Field>
          <Toggle checked={singleUse} onChange={setSingleUse} label="One guest session" hint="The guest confirms once, then browses for up to 15 minutes. Reusable invitations allow at most eight live sessions." testId="guest-single-use" />
          {configured && !settings.data?.gateway_available && <Callout tone="idle">The guest listener is unavailable. Check the server before creating invitations.</Callout>}
          <Button disabled={!configured || !settings.data?.gateway_available} busy={busy} onClick={() => void act(async () => setCreated(await client.createGuestInvitation(app, pr, ttl, singleUse)))} data-testid="create-guest-invitation">Create invitation (owners)</Button>
          {created && <Callout tone="idle"><div className="space-y-3"><p>This secret link is shown once. Anyone with the complete link can use it. Keep it out of public PR comments.</p><TextInput aria-label="Secret guest invitation" value={created.url} readOnly /><CopyButton text={created.url} label="Copy secret invitation" /></div></Callout>}
          {grants.data?.map(grant => <div key={grant.id} className="flex items-center justify-between gap-3 border-t border-line pt-3 text-xs" data-testid="guest-invitation-row">
            <div><p>{grant.revoked_at ? "Revoked" : grant.expires_at * 1000 <= Date.now() ? "Expired" : grant.single_use && grant.redeemed ? "Invitation used; a session may still be active" : "Available"} · {grant.single_use ? "One session" : "Reusable"}</p><p className="text-fg-muted">Expires {new Date(grant.expires_at * 1000).toLocaleString()} · generation {grant.deployment_id.slice(-8)}</p></div>
            {!grant.revoked_at && grant.expires_at * 1000 > Date.now() && <Button size="sm" disabled={busy} onClick={() => void act(async () => { await client.revokeGuestInvitation(grant.id); if (created?.grant.id === grant.id) setCreated(undefined); })}>Revoke</Button>}
          </div>)}
          <p className="text-xs text-fg-muted">Revoking ends existing sessions too. Downloaded content cannot be taken back. App Basic Auth still applies. Guest links currently support previews on the main server.</p>
          <details className="text-xs text-fg-muted"><summary className="cursor-pointer">Guest limits</summary><p className="mt-2">Sessions last at most 15 minutes. Uploads are limited to 16 MiB and individual transfers to 64 MiB. Browser workers and embedded frames are unavailable; use the private link when an app needs them.</p></details>
          <details className="space-y-3 border-t border-line pt-3"><summary className="cursor-pointer text-xs">Public guest domain settings (owners)</summary>
            <p className="text-xs text-fg-muted">Use a dedicated domain such as previews.example.com, with wildcard DNS pointing to this server or an HTTPS tunnel origin. Changing it revokes all guest invitations on this server.</p>
            <Field label="Guest domain"><TextInput aria-label="Guest domain" value={editDomain} onChange={e => setDomain(e.target.value)} maxLength={230} placeholder={settings.data?.config.domain ?? "previews.example.com"} /></Field>
            <Field label="Public HTTPS port"><TextInput aria-label="Public HTTPS port" value={editPort} onChange={e => setPort(e.target.value)} inputMode="numeric" /></Field>
            <Button size="sm" busy={busy} disabled={!editDomain.trim() || !Number.isInteger(Number(editPort)) || Number(editPort) < 1 || Number(editPort) > 65535} onClick={() => void act(() => client.configureGuestDomain(editDomain.trim(), Number(editPort)))}>Use guest domain</Button>
            {configured && <Button size="sm" disabled={busy} onClick={() => void act(async () => { await client.configureGuestDomain(null); setCreated(undefined); })}>Turn off guest invitations</Button>}
            <Button size="sm" disabled={busy} onClick={() => void act(async () => { await client.rotateGuestSecurity(); setCreated(undefined); })}>Revoke all sessions and reset guest clock</Button>
          </details>
          <ErrorNote error={error ?? settings.error ?? grants.error} />
        </div>
      </Card>}
    </div>
  </Modal>;
}
