import { GitPullRequest, Lock, MessageSquare, Plus } from "lucide-react";
import type { ReactNode } from "react";
import { Card, Dot, Reveal, Section, SectionHeader, Shot, cx } from "./ui";

function Feature({
  title,
  body,
  children,
  className,
  delay = 0,
}: {
  title: string;
  body: string;
  children: ReactNode;
  className?: string;
  delay?: number;
}) {
  return (
    <Reveal delay={delay} className={className}>
      <Card className="flex h-full flex-col">
        <div className="p-5 pb-0 sm:p-7 sm:pb-0">
          <h3 className="text-[17px] font-medium tracking-tight">{title}</h3>
          <p className="mt-2 max-w-[46ch] text-[14.5px] leading-relaxed text-fg-muted">{body}</p>
        </div>
        <div className="relative mt-6 flex-1 px-5 pb-5 sm:px-7 sm:pb-7">{children}</div>
      </Card>
    </Reveal>
  );
}

export function Features() {
  return (
    <Section id="features" className="pb-20 sm:pb-32">
      <SectionHeader
        eyebrow="Features"
        title="Everything you deploy with today."
        muted="Running on a machine you own, with nothing to pay."
      />
      <div className="mt-10 grid gap-4 [&>*]:min-w-0 sm:mt-14 md:grid-cols-2 lg:grid-cols-6">
        <Feature
          className="md:col-span-2 lg:col-span-4"
          title="Push to deploy"
          body="Push to a branch and your app builds and goes live. It works behind any home router: no webhooks, no open ports."
        >
          <Deployments />
        </Feature>
        <Feature
          className="lg:col-span-2"
          delay={0.06}
          title="A preview for every pull request"
          body="Each pull request gets its own URL, updated on every push and removed when it closes. Forks are never built."
        >
          <PreviewComment />
        </Feature>
        <Feature
          className="lg:col-span-2"
          title="Roll back in one click"
          body="The last five builds stay ready. If a new version comes up unhealthy, the previous one goes back live by itself."
        >
          <Rollback />
        </Feature>
        <Feature
          className="lg:col-span-2"
          delay={0.06}
          title="Databases in one click"
          body="PostgreSQL, MySQL, MongoDB, Redis and ClickHouse, with nightly encrypted backups."
        >
          <Databases />
        </Feature>
        <Feature
          className="lg:col-span-2"
          delay={0.12}
          title="Your domain, with HTTPS"
          body="One command connects your domain through Cloudflare. HTTPS works on any home connection, even behind CGNAT."
        >
          <Domain />
        </Feature>
        <Feature
          className="lg:col-span-3"
          title="A firewall in your repo"
          body="Block IPs, deny paths and rate-limit bots with a vercelify.json next to your code."
        >
          <Firewall />
        </Feature>
        <Feature
          className="lg:col-span-3"
          delay={0.06}
          title="Add machines as you grow"
          body="When one laptop is full, run the same installer on another, or on a Raspberry Pi. They all show up in one app."
        >
          <Machines />
        </Feature>
      </div>
    </Section>
  );
}

const panel = "rounded-xl border border-line bg-[#0e0e0d]";

function Deployments() {
  return (
    <Shot
      src="/app/deployments.jpg"
      alt="Deployment history for portfolio: production, a pull request preview, a failed build and rollbacks"
      pos="64% 50%"
      zoom={1.55}
      width="(min-width: 1024px) 680px, (min-width: 768px) 90vw, 100vw"
      className="aspect-[16/8]"
    />
  );
}

function PreviewComment() {
  return (
    <div className={cx(panel, "p-4 text-[13px]")}>
      <div className="flex items-center gap-2 text-fg-muted">
        <GitPullRequest className="size-4 text-accent-fg" />
        <span className="text-fg">New pricing page</span>
        <span className="text-fg-dim">#42</span>
      </div>
      <div className="mt-3 rounded-lg border border-line bg-surface p-3">
        <div className="flex items-center gap-2 text-[12px] text-fg-dim">
          <MessageSquare className="size-3.5" /> vercelify commented
        </div>
        <div className="mt-2 flex items-center gap-2">
          <Dot tone="ok" /> <span className="text-fg-muted">Preview ready</span>
        </div>
        <div className="mt-1.5 truncate font-mono text-[12px] text-accent-fg">pr-42-portfolio.you.dev</div>
      </div>
    </div>
  );
}

function Rollback() {
  return (
    <div className={cx(panel, "p-4")}>
      <div className="relative flex items-center justify-between px-1">
        <div className="absolute inset-x-3 top-1/2 h-px bg-line-strong" />
        {["2f5d", "2972", "2386", "1d9a", "11c3"].map((s, i) => (
          <div key={s} className="relative flex flex-col items-center gap-2">
            <span
              className={cx(
                "size-3 rounded-full border-2 border-[#0e0e0d]",
                i === 3 ? "bg-bad" : i === 2 ? "bg-ok ring-4 ring-ok/15" : "bg-fg-dim",
              )}
            />
          </div>
        ))}
      </div>
      <div className="mt-4 flex items-center justify-between text-[12px]">
        <span className="text-fg-muted">
          <span className="text-bad">1d9abba</span> unhealthy after 40s
        </span>
      </div>
      <div className="mt-1 text-[12px] text-fg-muted">
        <span className="text-ok">23867aa</span> back live automatically
      </div>
    </div>
  );
}

function Databases() {
  const dbs = [
    ["PostgreSQL", "running"],
    ["Redis", "running"],
    ["MySQL", ""],
    ["MongoDB", ""],
    ["ClickHouse", ""],
  ];
  return (
    <div className="flex flex-wrap gap-2">
      {dbs.map(([name, s]) => (
        <span
          key={name}
          className={cx(
            "inline-flex items-center gap-2 rounded-lg border px-3 py-2 text-[13px]",
            s ? "border-line-strong bg-surface-2 text-fg" : "border-dashed border-line-strong text-fg-dim",
          )}
        >
          {s ? <Dot tone="ok" /> : <Plus className="size-3" />}
          {name}
        </span>
      ))}
      <div className="mt-2 w-full font-mono text-[11.5px] text-fg-dim">Backups · nightly · encrypted · last 03:00</div>
    </div>
  );
}

function Domain() {
  return (
    <div className="space-y-2.5">
      <div className="flex items-center gap-2 rounded-lg border border-line-strong bg-surface-2 px-3 py-2.5 font-mono text-[12.5px]">
        <Lock className="size-3.5 text-ok" />
        <span className="text-fg-dim">https://</span>
        <span className="text-fg">you.dev</span>
      </div>
      <div className={cx(panel, "px-3 py-2.5 font-mono text-[11.5px] leading-relaxed text-fg-muted")}>
        <span className="text-accent">$</span> sudo vercelify domain you.dev
        <br />
        <span className="text-ok">✔</span> Tunnel up · *.you.dev live
      </div>
    </div>
  );
}

function Firewall() {
  return (
    <pre className={cx(panel, "overflow-x-auto p-4 font-mono text-[11.5px] leading-[1.7] text-fg-muted")}>
      <span className="text-fg-dim">{"// vercelify.json"}</span>
      {"\n{\n  "}
      <span className="text-accent-fg">"firewall"</span>
      {": {\n    "}
      <span className="text-accent-fg">"blockIps"</span>
      {": ["}
      <span className="text-accent-soft">"203.0.113.0/24"</span>
      {"],\n    "}
      <span className="text-accent-fg">"rules"</span>
      {": [\n      { "}
      <span className="text-accent-fg">"path"</span>: <span className="text-accent-soft">"/wp-admin/(.*)"</span>, <span className="text-accent-fg">"action"</span>
      {": "}
      <span className="text-accent-soft">"deny"</span>
      {" },\n      { "}
      <span className="text-accent-fg">"path"</span>: <span className="text-accent-soft">"/api/(.*)"</span>, <span className="text-accent-fg">"action"</span>
      {": "}
      <span className="text-accent-soft">"rate_limit"</span>
      {", "}
      <span className="text-accent-fg">"requests"</span>
      {": 60 }\n    ]\n  }\n}"}
    </pre>
  );
}

function Machines() {
  return (
    <Shot
      src="/app/servers.jpg"
      alt="Servers page: a laptop with CPU, memory, disks, temperature and the workers it deploys to"
      pos="44% 40%"
      zoom={1.9}
      className="aspect-[16/9]"
    />
  );
}
