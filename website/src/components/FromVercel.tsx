import { Check } from "lucide-react";
import { useState } from "react";
import { Card, Reveal, Section, SectionHeader, cx } from "./ui";

const RULES = [
  ["redirects, rewrites, headers", "Applied by the proxy, including single-page app fallbacks"],
  ["cleanUrls, trailingSlash", "/about.html becomes /about, as on Vercel"],
  ["crons", "Called on schedule with your CRON_SECRET, so cron code needs no change"],
  ["Next.js middleware, ISR, next/image", "Run in your app's own server: no cold starts, no time limits"],
] as const;

export function FromVercel() {
  const [tab, setTab] = useState<"json" | "cli">("json");
  return (
    <Section id="vercel" className="pb-20 sm:pb-32">
      <div className="grid items-start gap-12 [&>*]:min-w-0 lg:grid-cols-[0.9fr_1.1fr] lg:gap-16">
        <div>
          <SectionHeader
            eyebrow="Coming from Vercel"
            size="md"
            title="Keep your vercel.json."
            muted="Vercelify applies it on every deploy."
          />
          <Reveal delay={0.1}>
            <ul className="mt-10 divide-y divide-line border-y border-line">
              {RULES.map(([k, v]) => (
                <li key={k} className="flex gap-4 py-4">
                  <Check className="mt-0.5 size-4 shrink-0 text-accent" />
                  <div>
                    <div className="font-mono text-[13px] text-fg">{k}</div>
                    <div className="mt-1 text-[14px] text-fg-muted">{v}</div>
                  </div>
                </li>
              ))}
            </ul>
            <p className="mt-6 text-[14px] leading-relaxed text-fg-dim">
              Next.js, React, Vue, Svelte, Astro, Node, Python, Go, PHP, static sites and anything with a Dockerfile build
              with no setup.
            </p>
          </Reveal>
        </div>

        <Reveal delay={0.12} className="lg:sticky lg:top-24">
          <Card className="bg-[#0d0d0c]">
            <div className="flex items-center gap-1 border-b border-line p-2">
              {(
                [
                  ["json", "vercel.json"],
                  ["cli", "vfy in your terminal"],
                ] as const
              ).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  onClick={() => setTab(id)}
                  className={cx(
                    "rounded-md px-3 py-1.5 font-mono text-[12px] transition-colors",
                    tab === id ? "bg-surface-3 text-fg" : "text-fg-dim hover:text-fg-muted",
                  )}
                >
                  {label}
                </button>
              ))}
            </div>
            {tab === "json" ? <JsonView /> : <CliView />}
          </Card>
        </Reveal>
      </div>
    </Section>
  );
}

const k = (s: string) => <span className="text-accent-fg">"{s}"</span>;
const v = (s: string) => <span className="text-accent-soft">"{s}"</span>;

function JsonView() {
  return (
    <div>
      <pre className="overflow-x-auto p-4 font-mono text-[11.5px] leading-[1.75] text-fg-muted sm:p-5 sm:text-[12.5px]">
        {"{\n  "}
        {k("cleanUrls")}: <span className="text-fg">true</span>
        {",\n  "}
        {k("redirects")}
        {": [{ "}
        {k("source")}: {v("/blog/:slug")}, {k("destination")}: {v("/posts/:slug")}
        {" }],\n  "}
        {k("rewrites")}
        {": [{ "}
        {k("source")}: {v("/(.*)")}, {k("destination")}: {v("/index.html")}
        {" }],\n  "}
        {k("crons")}
        {": [{ "}
        {k("path")}: {v("/api/digest")}, {k("schedule")}: {v("0 8 * * *")}
        {" }]\n}"}
      </pre>
      <div className="border-t border-line px-5 py-4">
        <div className="font-mono text-[11px] uppercase tracking-[0.08em] text-fg-dim">Applied on deploy 11c33da</div>
        <div className="mt-3 grid gap-2 font-mono text-[12px] sm:grid-cols-2">
          {["1 redirect", "1 rewrite (SPA fallback)", "Clean URLs on", "Cron · next run 08:00 UTC"].map((t) => (
            <span key={t} className="flex items-center gap-2 text-fg-muted">
              <span className="size-1.5 rounded-full bg-ok" /> {t}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}

function CliView() {
  const lines = [
    ["vfy apps", "your apps and whether they're up"],
    ["vfy deploy my-app", "deploy now"],
    ["vfy logs my-app", "what your app is printing"],
    ["vfy rollback my-app 4b1d2e7", "put an earlier version back"],
    ["vfy env pull my-app", "its settings into a .env file"],
    ["vfy config check", "what this folder's vercel.json would do"],
  ];
  return (
    <div className="p-4 font-mono text-[12px] leading-[2] sm:p-5 sm:text-[12.5px]">
      {lines.map(([c, d]) => (
        <div key={c} className="flex flex-wrap gap-x-4">
          <span>
            <span className="text-accent">$ </span>
            <span className="text-fg">{c}</span>
          </span>
          <span className="text-fg-dim"># {d}</span>
        </div>
      ))}
      <p className="mt-4 font-sans text-[13px] leading-relaxed text-fg-dim">
        Uses the servers you paired in the app. Nothing to log in to.
      </p>
    </div>
  );
}
