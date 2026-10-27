import { Globe, Laptop } from "lucide-react";
import { Mark, Reveal, Section, SectionHeader, Shot, cx } from "./ui";

const CARE = [
  ["Builds run one at a time", "They pause when the CPU runs hot, so the apps already live stay fast."],
  ["Idle apps sleep", "The next visit wakes them in a second or two, so memory goes to what's in use."],
  ["Power cuts are handled", "On battery and running low, it shuts down cleanly, then starts your apps when power returns."],
  ["Close the lid", "It keeps serving. Lid, sleep and screen settings are configured during install."],
  ["Fast disk, big disk", "Move a database between SSD and HDD with its data and permissions intact."],
] as const;

export function Hardware() {
  return (
    <Section id="hardware" className="pb-20 sm:pb-32">
      <div className="grid items-center gap-12 [&>*]:min-w-0 lg:grid-cols-[1.05fr_0.95fr] lg:gap-16">
        <Reveal className="order-2 lg:order-1">
          <Health />
        </Reveal>
        <div className="order-1 lg:order-2">
          <SectionHeader
            eyebrow="Made for old hardware"
            size="md"
            title="Built for an i5 and 8 GB of RAM."
            muted="It watches heat, power and memory for you."
          />
          <Reveal delay={0.1}>
            <ul className="mt-10 space-y-5">
              {CARE.map(([t, d]) => (
                <li key={t} className="flex gap-4">
                  <span className="mt-2 size-1.5 shrink-0 rounded-[1px] bg-accent" />
                  <p className="text-[15px] leading-relaxed text-fg-muted">
                    <span className="text-fg">{t}.</span> {d}
                  </p>
                </li>
              ))}
            </ul>
          </Reveal>
        </div>
      </div>
    </Section>
  );
}

function Health() {
  return (
    <Shot
      src="/app/health.jpg"
      alt="Health page: CPU, memory, disk and temperature, a 24-hour chart and the power and thermal guard"
      pos="100% 10%"
      zoom={1.06}
      className="aspect-[4/3] shadow-[0_40px_100px_-40px_rgb(0_0_0/0.9)]"
    />
  );
}

/** How the pieces connect: Mac ↔ mesh ↔ laptop ↔ tunnel ↔ visitors. */
export function Network() {
  return (
    <Section id="network" className="pb-20 sm:pb-32">
      <SectionHeader
        align="center"
        eyebrow="How it connects"
        title="Private by default."
        muted="Public only when you give an app a domain."
      />
      <Reveal delay={0.1} className="mt-10 sm:mt-16">
        <div className="relative overflow-hidden rounded-3xl border border-line bg-panel/40 px-4 py-8 sm:px-10 sm:py-12">
          <div aria-hidden className="dot-grid absolute inset-0 [mask-image:radial-gradient(ellipse_at_center,#000,transparent_75%)]" />
          <div className="relative flex flex-col items-stretch lg:flex-row lg:items-center">
            <Node icon={<Laptop className="size-5" />} title="Your Mac" lines={["Vercelify app", "vfy command line"]} />
            <Link label="vercelify-mesh" note="private, encrypted" delay="0s" />
            <Node
              hot
              icon={<Mark className="h-5" />}
              title="Your old laptop"
              lines={["Vercelify agent and engine", "Traefik · your apps · databases"]}
            />
            <Link label="Cloudflare Tunnel" note="optional, for a domain" delay="1.2s" />
            <Node icon={<Globe className="size-5" />} title="Visitors" lines={["https://you.dev", "cached at Cloudflare's edge"]} />
          </div>
          <p className="relative mx-auto mt-8 max-w-[640px] sm:mt-12 text-center text-[14.5px] leading-relaxed text-fg-muted">
            The control panel is reachable only on Vercelify's own WireGuard network, never from the public internet. No
            ports are opened on your router, and there is no account to sign up for.
          </p>
        </div>
      </Reveal>
    </Section>
  );
}

function Node({ icon, title, lines, hot }: { icon: React.ReactNode; title: string; lines: string[]; hot?: boolean }) {
  return (
    <div
      className={cx(
        "relative z-10 min-w-0 rounded-2xl border bg-surface p-5 lg:flex-1",
        hot ? "border-accent/35 shadow-[0_0_0_4px_rgb(255_106_61/0.06),0_20px_60px_-20px_rgb(255_74_43/0.45)]" : "border-line-strong",
      )}
    >
      <div className={cx("grid size-10 place-items-center rounded-xl border border-line-strong bg-surface-2", hot ? "text-accent" : "text-fg-muted")}>
        {icon}
      </div>
      <div className="mt-4 text-[15px] font-medium">{title}</div>
      {lines.map((l) => (
        <div key={l} className="mt-1 font-mono text-[11.5px] text-fg-dim">
          {l}
        </div>
      ))}
    </div>
  );
}

function Link({ label, note, delay }: { label: string; note: string; delay: string }) {
  return (
    <div className="relative flex min-h-[96px] items-center justify-center sm:min-h-[112px] lg:min-h-[132px] lg:w-[172px] lg:shrink-0">
      {/* Vertical on small screens, horizontal on large. The label sits above
          the wire and the note below it, so neither covers the cards. */}
      <div className="wire top-0 bottom-0 left-1/2 lg:hidden" />
      <div className="absolute inset-x-0 top-1/2 hidden h-px overflow-hidden bg-line-strong lg:block">
        <span className="packet-x" style={{ ["--delay" as string]: delay }} />
      </div>
      <span className="absolute top-1/2 left-0 hidden size-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-line-strong lg:block" />
      <span className="absolute top-1/2 right-0 hidden size-1.5 translate-x-1/2 -translate-y-1/2 rounded-full bg-line-strong lg:block" />
      <div className="relative z-10 bg-bg px-3 py-1.5 text-center lg:bg-transparent lg:p-0">
        <div className="font-mono text-[11px] whitespace-nowrap text-fg lg:absolute lg:bottom-3 lg:left-1/2 lg:-translate-x-1/2">
          {label}
        </div>
        <div className="text-[10.5px] whitespace-nowrap text-fg-dim lg:absolute lg:top-3 lg:left-1/2 lg:-translate-x-1/2">
          {note}
        </div>
      </div>
    </div>
  );
}
