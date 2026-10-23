import { GitBranch, Pause, Play, RefreshCw, Rocket, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { agentOn, coolifyOn, defaultTarget, type NewApp } from "../lib/api";
import { ago, bytes, shortSha } from "../lib/format";
import { cloneUrl, repoName, validBranch } from "../lib/git";
import { usePoll } from "../lib/hooks";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import type { AutoDeployStatus, AutoDeployWatch, CockpitSettings, CoolifyApplication, CoolifyServer } from "../types";
import { useToast } from "./Toast";
import { Button, Callout, cx, ErrorNote, Field, InlineCode, Modal, OrbitButton, Select, StatusDot, TextInput, Tooltip } from "./ui";

const BUILD_PACKS: { value: NewApp["build_pack"]; label: string; port: string }[] = [
  { value: "nixpacks", label: "Detect automatically (Nixpacks)", port: "3000" },
  { value: "static", label: "Static site", port: "80" },
  { value: "dockerfile", label: "Dockerfile in the repo", port: "3000" },
  { value: "dockercompose", label: "docker-compose.yml in the repo", port: "3000" },
];

/** Create an app from a public Git repository and keep it deployed from a branch. */
/** The paired machine behind a Coolify server: localhost is this server, workers carry their hostname. */
function pairedFor(c: CoolifyServer, current: CockpitSettings, paired: CockpitSettings[]): CockpitSettings | undefined {
  if (c.ip === "host.docker.internal" || c.name === "localhost") return current;
  return paired.find((p) => p.hostname === c.name);
}

/** Free memory per Coolify server, where we can tell (placement hints). */
async function freeMemory(list: CoolifyServer[], current: CockpitSettings, paired: CockpitSettings[]): Promise<Record<string, number>> {
  const out: Record<string, number> = {};
  await Promise.all(
    list.map(async (c) => {
      const p = pairedFor(c, current, paired);
      if (!p) return;
      try {
        out[c.uuid] = (await agentOn(p.agent_url).system()).memory.available_bytes;
      } catch {
        /* unreachable: no hint */
      }
    }),
  );
  return out;
}

export function NewAppModal({ open, onClose, onCreated }: { open: boolean; onClose: () => void; onCreated: (uuid: string) => void }) {
  const toast = useToast();
  const session = useSession();
  const agent = agentOn(session.settings.agent_url);
  const coolify = coolifyOn(session.settings.agent_url);
  const servers = usePoll(() => (open ? coolify.servers() : Promise.resolve([])), 0, [open]);
  const free = usePoll(
    () => (servers.data?.length ? freeMemory(servers.data, session.settings, session.servers) : Promise.resolve({} as Record<string, number>)),
    0,
    [servers.data],
  );
  const roomiest = Object.entries(free.data ?? {}).sort((a, b) => b[1] - a[1])[0]?.[0];
  const [repo, setRepo] = useState("");
  const [branch, setBranch] = useState("main");
  const [name, setName] = useState("");
  const [pack, setPack] = useState<NewApp["build_pack"]>("nixpacks");
  const [port, setPort] = useState("3000");
  const [server, setServer] = useState("");
  const [auto, setAuto] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  const url = cloneUrl(repo);
  useEffect(() => {
    const list = servers.data ?? [];
    if (!list.some((s) => s.uuid === server)) setServer((list.find((s) => s.ip === "host.docker.internal") ?? list[0])?.uuid ?? "");
  }, [servers.data, server]);

  async function create() {
    if (!url) return setError("Enter the repository's https:// address, e.g. https://github.com/you/app");
    if (!validBranch(branch)) return setError(`“${branch}” is not a valid branch name`);
    setBusy(true);
    setError(undefined);
    try {
      if (!server) throw new Error("Coolify has no servers yet");
      const target = await defaultTarget(coolify);
      const appName = name.trim() || repoName(url);
      const created = await coolify.createPublicApp({
        ...target,
        server_uuid: server,
        git_repository: url,
        git_branch: branch,
        build_pack: pack,
        ports_exposes: port.trim() || "3000",
        name: appName,
        instant_deploy: true,
      });
      if (auto) {
        await agent.autodeploySave({ app_uuid: created.uuid, name: appName, repo: url, branch });
        // Record the current head now, so a push made during the first build is not missed.
        await agent.autodeployCheck(created.uuid).catch(() => {});
      }
      toast.ok(`${appName} is building${auto ? ` — pushes to ${branch} deploy automatically` : ""}`);
      onCreated(created.uuid);
      setRepo("");
      setName("");
    } catch (e) {
      setError(toCockpitError(e).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open={open} onClose={onClose} title="New app from Git" description="Coolify builds it on your server and gives it a URL.">
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <Field label="Repository" hint="A public repository on GitHub, GitLab or any Git host">
          <TextInput value={repo} onChange={(e) => setRepo(e.target.value)} placeholder="https://github.com/you/app" spellCheck={false} autoFocus data-testid="new-app-repo" />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Branch">
            <TextInput value={branch} onChange={(e) => setBranch(e.target.value.trim())} spellCheck={false} data-testid="new-app-branch" />
          </Field>
          <Field label="Name">
            <TextInput value={name} onChange={(e) => setName(e.target.value)} placeholder={url ? repoName(url) : "my-app"} spellCheck={false} />
          </Field>
          <Field label="Build">
            <Select
              value={pack}
              onChange={(e) => {
                const p = e.target.value as NewApp["build_pack"];
                setPack(p);
                setPort(BUILD_PACKS.find((b) => b.value === p)!.port);
              }}
            >
              {BUILD_PACKS.map((b) => (
                <option key={b.value} value={b.value}>
                  {b.label}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="Port the app listens on">
            <TextInput value={port} onChange={(e) => setPort(e.target.value.replace(/[^0-9,]/g, ""))} inputMode="numeric" />
          </Field>
        </div>
        {(servers.data?.length ?? 0) > 1 && (
          <Field label="Run on" hint="Workers you added to this server's pool show up here">
            <Select value={server} onChange={(e) => setServer(e.target.value)} data-testid="new-app-server">
              {servers.data!.map((s) => (
                <option key={s.uuid} value={s.uuid}>
                  {s.name}
                  {free.data?.[s.uuid] != null ? ` · ${bytes(free.data[s.uuid])} free${s.uuid === roomiest ? " (most room)" : ""}` : ""}
                  {s.settings?.is_reachable === false ? " (unreachable)" : ""}
                </option>
              ))}
            </Select>
          </Field>
        )}
        <label className="flex cursor-pointer items-start gap-2.5 rounded-lg border border-line bg-surface-2 px-3 py-2.5 text-[13px]">
          <input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} className="mt-0.5 accent-[var(--accent)]" data-testid="new-app-auto" />
          <span>
            <span className="font-medium text-fg">Deploy automatically when I push to {branch || "the branch"}</span>
            <span className="mt-0.5 block text-xs text-fg-muted">The server checks the branch every minute. No webhook or open port needed.</span>
          </span>
        </label>
        <p className="text-xs text-fg-dim">
          Private repository? Connect GitHub in Coolify (Sources), create the app there, then turn on auto-deploy in its <InlineCode>Auto-deploy</InlineCode> tab here.
        </p>
        <ErrorNote error={error ? { message: error } : undefined} />
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <OrbitButton type="submit" icon={<Rocket />} busy={busy} busyLabel="Creating" disabled={!repo.trim()} testId="new-app-create">
            Create & deploy
          </OrbitButton>
        </div>
      </form>
    </Modal>
  );
}

/** The auto-deploy tab of an app. */
export function AutoDeployPanel({
  app,
  status,
  refresh,
}: {
  app: CoolifyApplication;
  status: AutoDeployStatus | undefined;
  refresh: () => Promise<void>;
}) {
  const toast = useToast();
  const { settings } = useSession();
  const agent = agentOn(settings.agent_url);
  const watch = status?.watches.find((w) => w.app_uuid === app.uuid);
  const [repo, setRepo] = useState(watch?.repo ?? cloneUrl(app.git_repository) ?? "");
  const [branch, setBranch] = useState(watch?.branch ?? app.git_branch ?? "main");
  const [token, setToken] = useState("");
  const [previews, setPreviews] = useState(watch?.previews ?? false);
  const [feedback, setFeedback] = useState(watch?.github_feedback ?? false);
  const [autoRollback, setAutoRollback] = useState(watch?.auto_rollback ?? true);
  const [busy, setBusy] = useState<string>();
  const [error, setError] = useState<string>();
  const github = /^https:\/\/github\.com\/[^/]+\/[^/]+/.test(repo.trim());

  async function run(label: string, fn: () => Promise<AutoDeployWatch | unknown>, ok?: string) {
    setBusy(label);
    setError(undefined);
    try {
      await fn();
      if (ok) toast.ok(ok);
      await refresh();
    } catch (e) {
      setError(toCockpitError(e).message);
    } finally {
      setBusy(undefined);
    }
  }

  const save = (enabled = true) =>
    run(
      enabled ? "save" : "pause",
      async () => {
        const w = await agent.autodeploySave({
          app_uuid: app.uuid,
          name: app.name,
          repo,
          branch,
          enabled,
          previews: github && previews,
          github_feedback: github && feedback,
          auto_rollback: autoRollback,
          ...(token ? { token } : {}),
        });
        setToken("");
        if (enabled && !w.last_commit) await agent.autodeployCheck(app.uuid);
        return w;
      },
      enabled ? `Pushes to ${branch} now deploy ${app.name}` : `Auto-deploy paused for ${app.name}`,
    );

  const every = status?.interval_secs ? `every ${status.interval_secs < 120 ? `${status.interval_secs} s` : `${Math.round(status.interval_secs / 60)} min`}` : "on demand";

  return (
    <div className="space-y-4" data-testid="autodeploy">
      {status && !status.coolify_configured && (
        <Callout title="The server has no Coolify API token">
          Run <InlineCode>sudo vercelify pair --refresh</InlineCode> on it so the agent can start deployments.
        </Callout>
      )}
      {watch && (
        <div className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line text-[13px] lg:grid-cols-4">
          <Cell label="Status">
            <span className="inline-flex items-center gap-1.5">
              <StatusDot health={!watch.enabled ? "idle" : watch.last_error ? "warn" : "ok"} />
              {watch.enabled ? `Watching ${watch.branch}` : "Paused"}
            </span>
          </Cell>
          <Cell label="Last check">{ago(watch.last_checked_at)}</Cell>
          <Cell label="Branch head">
            <code className="font-mono text-[12px]">{shortSha(watch.last_commit) || "—"}</code>
          </Cell>
          <Cell label="Auto deploys">
            {watch.deploys}
            {watch.last_deployed_at && <span className="text-fg-dim"> · last {ago(watch.last_deployed_at)}</span>}
          </Cell>
        </div>
      )}
      {watch?.last_error && <Callout tone="bad">{watch.last_error}</Callout>}

      <form
        className="space-y-3"
        onSubmit={(e) => {
          e.preventDefault();
          void save(true);
        }}
      >
        <div className="grid grid-cols-[1fr_200px] gap-3">
          <Field label="Repository">
            <TextInput value={repo} onChange={(e) => setRepo(e.target.value)} placeholder="https://github.com/you/app.git" spellCheck={false} data-testid="autodeploy-repo" />
          </Field>
          <Field label="Branch">
            <TextInput value={branch} onChange={(e) => setBranch(e.target.value.trim())} spellCheck={false} data-testid="autodeploy-branch" />
          </Field>
        </div>
        <Field
          label="Access token (private repositories only)"
          hint={
            watch?.has_token
              ? "A token is saved on the server. Leave this empty to keep it."
              : "A read-only token, e.g. a GitHub fine-grained token with Contents: read. Stored on the server only."
          }
        >
          <TextInput type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" placeholder={watch?.has_token ? "••••••••" : ""} />
        </Field>
        <fieldset className="space-y-2 rounded-lg border border-line p-3">
          <legend className="px-1 text-[11px] text-fg-dim">Like Vercel</legend>
          <Toggle
            checked={autoRollback}
            onChange={setAutoRollback}
            label="Roll back automatically"
            hint="If the new version is unhealthy right after deploying, the previous one goes back live (instantly, from its kept image)."
            testId="autodeploy-rollback"
          />
          <Toggle
            checked={github && previews}
            onChange={setPreviews}
            disabled={!github}
            label="Preview every pull request"
            hint={
              github
                ? "Each open pull request gets its own URL and is removed when it closes. Pull requests from forks are never built (someone else's code)."
                : "GitHub repositories only."
            }
            testId="autodeploy-previews"
          />
          <Toggle
            checked={github && feedback}
            onChange={setFeedback}
            disabled={!github}
            label="Report to GitHub"
            hint="Commit statuses on every deploy and a comment with the preview URL. Needs a token with Commit statuses and Pull requests write access."
            testId="autodeploy-feedback"
          />
        </fieldset>
        {watch && watch.open_previews.length > 0 && (
          <ul className="divide-y divide-line rounded-lg border border-line text-[13px]" data-testid="autodeploy-open-previews">
            {watch.open_previews.map((p) => (
              <li key={p.number} className="flex items-center gap-3 px-3 py-2">
                <StatusDot health="ok" />
                <span className="w-12 shrink-0 font-mono text-xs text-fg-muted">#{p.number}</span>
                <span className="min-w-0 flex-1 truncate">{p.title}</span>
                {p.url && (
                  <a href={p.url} target="_blank" rel="noreferrer" className="shrink-0 truncate font-mono text-xs text-fg-muted underline-offset-4 hover:text-fg hover:underline">
                    {p.url.replace(/^https?:\/\//, "")}
                  </a>
                )}
                <span className="shrink-0 text-xs text-fg-dim">{ago(p.deployed_at)}</span>
              </li>
            ))}
          </ul>
        )}
        {watch && watch.skipped > 0 && (
          <p className="text-xs text-fg-dim" data-testid="autodeploy-skipped">
            {watch.skipped} push{watch.skipped === 1 ? " was" : "es were"} not built because nothing for this app changed
            {watch.last_skip ? <> (last: {watch.last_skip})</> : null}. Set the app’s watch paths in Coolify, or end a commit message with{" "}
            <span className="font-mono">[skip deploy]</span>.
          </p>
        )}
        {watch && watch.rollbacks > 0 && (
          <p className="text-xs text-fg-dim">
            Rolled back automatically {watch.rollbacks} time{watch.rollbacks === 1 ? "" : "s"}. The next push deploys normally again.
          </p>
        )}
        <ErrorNote error={error ? { message: error } : undefined} />
        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line pt-3">
          <span className="text-xs text-fg-dim">
            The server asks Git for the head of <span className="font-mono">{branch || "…"}</span> {every} and deploys new commits. It skips deploys while the
            power guard is cooling the machine down.
          </span>
          <div className="flex items-center gap-1.5">
            {watch && (
              <>
                <Button size="sm" variant="ghost" icon={<Trash2 />} busy={busy === "remove"} onClick={() => void run("remove", () => agent.autodeployRemove(app.uuid), "Auto-deploy removed")}>
                  Remove
                </Button>
                <Button size="sm" icon={<RefreshCw />} busy={busy === "check"} onClick={() => void run("check", () => agent.autodeployCheck(app.uuid))} data-testid="autodeploy-check">
                  Check now
                </Button>
                {watch.enabled ? (
                  <Button size="sm" icon={<Pause />} busy={busy === "pause"} onClick={() => void save(false)}>
                    Pause
                  </Button>
                ) : null}
              </>
            )}
            <Button type="submit" size="sm" variant="primary" icon={watch?.enabled ? <GitBranch /> : <Play />} busy={busy === "save"} disabled={!repo.trim() || !branch} data-testid="autodeploy-save">
              {watch ? (watch.enabled ? "Save" : "Resume") : "Turn on auto-deploy"}
            </Button>
          </div>
        </div>
      </form>
    </div>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  hint,
  disabled,
  testId,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint: string;
  disabled?: boolean;
  testId?: string;
}) {
  return (
    <label className={cx("flex cursor-pointer items-start gap-2.5 text-[13px]", disabled && "cursor-not-allowed opacity-50")}>
      <input
        type="checkbox"
        className="mt-0.5 size-3.5 accent-[var(--fg)]"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        data-testid={testId}
      />
      <span>
        <span className="block font-medium">{label}</span>
        <span className="block text-xs text-fg-muted">{hint}</span>
      </span>
    </label>
  );
}

function Cell({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="min-w-0 bg-surface px-3.5 py-2.5">
      <div className="text-[11px] text-fg-dim">{label}</div>
      <div className="mt-0.5 truncate text-fg">{children}</div>
    </div>
  );
}

/** Quiet marker beside an app's name: an icon and a word, coloured only on errors. */
export function AutoBadge({ watch }: { watch?: AutoDeployWatch }) {
  if (!watch?.enabled) return null;
  return (
    <Tooltip content={watch.last_error ? `Auto-deploy: ${watch.last_error}` : `Deploys every push to ${watch.branch}`}>
      <span className={cx("inline-flex shrink-0 items-center gap-1 text-[11px]", watch.last_error ? "text-warn" : "text-fg-dim")}>
        <GitBranch className="size-3" />
        auto
      </span>
    </Tooltip>
  );
}
