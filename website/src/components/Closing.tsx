import { ArrowRight, Plus } from "lucide-react";
import { m, useInView, useScroll, useTransform } from "framer-motion";
import { useRef, useState } from "react";
import { LaserLanding, useShader } from "./LaserLanding";
import { Button, CopyCommand, DOCS, Dot, Eyebrow, GithubIcon, INSTALL_HOST, Logo, REPO, Reveal, Section, SectionHeader, cx } from "./ui";

const ROWS = [
  ["Monthly cost", "Free tier, then usage-based", "$0. You pay for electricity"],
  ["Request time limits", "Per-function limits", "None. Your app is a normal server"],
  ["Cold starts", "On idle functions", "None while running"],
  ["Where your data lives", "The provider's cloud", "Your own disk"],
  ["Global edge network", "Built in", "Cloudflare caches static files; your laptop is the origin"],
  ["Rolling releases", "Available", "Not offered: two versions at once would double memory use"],
  ["Uptime", "The provider's SLA", "As reliable as your power and internet"],
] as const;

export function Compare() {
  return (
    <Section id="compare" className="pb-20 sm:pb-32">
      <SectionHeader eyebrow="Compare" title="An honest comparison." muted="What you gain, and what you give up." />
      <Reveal delay={0.1} className="mt-10 overflow-hidden rounded-2xl border border-line sm:mt-12">
        <dl className="divide-y divide-line sm:hidden">
          {ROWS.map(([label, them, us]) => (
            <div key={label} className="px-4 py-4">
              <dt className="text-[14px] text-fg">{label}</dt>
              <dd className="mt-2 grid grid-cols-[88px_1fr] gap-x-3 gap-y-1.5 text-[13.5px] leading-snug">
                <span className="font-mono text-[10.5px] uppercase tracking-[0.08em] leading-5 text-fg-dim">Serverless</span>
                <span className="text-fg-dim">{them}</span>
                <span className="font-mono text-[10.5px] uppercase tracking-[0.08em] leading-5 text-accent-fg">Vercelify</span>
                <span className="text-fg-muted">{us}</span>
              </dd>
            </div>
          ))}
        </dl>
        <table className="hidden w-full text-left text-[14px] sm:table">
          <thead>
            <tr className="border-b border-line bg-panel/70 font-mono text-[11px] uppercase tracking-[0.08em]">
              <th className="px-4 py-4 font-normal text-fg-dim md:px-6" />
              <th className="px-4 py-4 font-normal text-fg-dim md:px-6">Hosted serverless</th>
              <th className="px-4 py-4 font-normal text-accent-fg md:px-6">Vercelify</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-line">
            {ROWS.map(([label, them, us]) => (
              <tr key={label}>
                <td className="px-4 py-4 text-fg md:px-6">{label}</td>
                <td className="px-4 py-4 text-fg-dim md:px-6">{them}</td>
                <td className="bg-accent/[0.03] px-4 py-4 text-fg-muted md:px-6">{us}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Reveal>
    </Section>
  );
}

const FAQ = [
  ["Is it really free?", "Yes. Every part is open source under the MIT license. The only optional cost is a domain name."],
  [
    "Will my Vercel project work?",
    "Usually without changes. Your app runs as a normal server instead of serverless functions, and your vercel.json redirects, rewrites, headers and crons are applied for you.",
  ],
  [
    "Is 8 GB of RAM enough?",
    "Yes, for several apps and their databases. Builds run one at a time, so the machine is never overwhelmed. 4 GB works for lighter use.",
  ],
  [
    "Do I need a domain?",
    "No. Without one, your apps are private to your own devices on Tailscale. Add a domain whenever you want them public.",
  ],
  [
    "Is it safe to run at home?",
    "The control panel is only reachable on your private Tailscale network. No router ports are opened, and apps go public only through a Cloudflare Tunnel you set up.",
  ],
  [
    "What if the power goes out?",
    "On a laptop, the battery covers short cuts. If it runs low, Vercelify shuts down cleanly and brings your apps back when power returns.",
  ],
  [
    "I don't have a Mac.",
    "Everything also works from the Coolify dashboard in any browser. Install Tailscale on your computer or phone and open the dashboard link the installer prints.",
  ],
  [
    "Which hardware works?",
    "Any 64-bit Intel or AMD PC or laptop with 20 GB of free disk, running Ubuntu 22.04 or 24.04, or Debian 12 or 13. Raspberry Pi 4 and 5 work as extra machines.",
  ],
] as const;

export function Faq() {
  const [open, setOpen] = useState<number | null>(0);
  return (
    <Section id="faq" className="pb-20 sm:pb-32">
      <div className="grid gap-12 [&>*]:min-w-0 lg:grid-cols-[0.8fr_1.2fr] lg:gap-16">
        <SectionHeader eyebrow="FAQ" size="md" title="Questions," muted="answered plainly." />
        <Reveal delay={0.1}>
          <div className="divide-y divide-line border-y border-line">
            {FAQ.map(([q, a], i) => {
              const isOpen = open === i;
              return (
                <div key={q}>
                  <button
                    type="button"
                    onClick={() => setOpen(isOpen ? null : i)}
                    aria-expanded={isOpen}
                    className="flex w-full items-center justify-between gap-4 py-4 text-left text-[15px] transition-colors hover:text-fg sm:gap-6 sm:py-5 sm:text-[16px]"
                  >
                    <span className={isOpen ? "text-fg" : "text-fg-muted"}>{q}</span>
                    <Plus className={cx("size-4 shrink-0 text-fg-dim transition-transform duration-300", isOpen && "rotate-45 text-accent")} />
                  </button>
                  <div className={cx("grid transition-[grid-template-rows] duration-300", isOpen ? "grid-rows-[1fr]" : "grid-rows-[0fr]")}>
                    <p className="overflow-hidden pr-6 text-[14.5px] leading-relaxed text-fg-muted sm:pr-10 sm:text-[15px]">
                      <span className="block pb-5">{a}</span>
                    </p>
                  </div>
                </div>
              );
            })}
          </div>
        </Reveal>
      </div>
    </Section>
  );
}

/** The last call to action: big type, one command, two ways forward. */
export function FinalCta() {
  return (
    <Section className="pt-4 pb-20 sm:pt-16 sm:pb-28">
      <Reveal>
        <Eyebrow>Get started</Eyebrow>
        <h2 className="mt-6 max-w-[1060px] text-[clamp(38px,5.8vw,84px)] sm:mt-8 leading-[0.95] font-medium tracking-[-0.045em] text-balance">
          <span className="text-headline">The laptop in your drawer </span>
          <span className="serif text-brand block pt-[0.04em] text-[1.1em] leading-[0.95]">is a server now.</span>
        </h2>
      </Reveal>
      <Reveal delay={0.1} className="mt-10 grid gap-8 border-t border-line pt-8 sm:mt-14 sm:gap-10 sm:pt-10 lg:grid-cols-[1fr_1.15fr] lg:gap-16">
        <p className="max-w-[420px] text-[17px] leading-[1.6] text-fg-muted">
          Install takes about 30 minutes, most of it downloads. No card, no domain and no router settings.
        </p>
        <div className="min-w-0">
          <CopyCommand command={INSTALL_HOST} display="curl -fsSL …/vercelify-oss/main/install.sh | sudo bash" />
          <div className="mt-4 flex flex-wrap gap-3">
            <Button href={DOCS}>
              Read the setup guide <ArrowRight className="size-3.5" />
            </Button>
            <Button href={REPO} variant="ghost">
              <GithubIcon className="size-3.5" /> Star on GitHub
            </Button>
          </div>
        </div>
      </Reveal>
    </Section>
  );
}

const FOOTER = [
  ["Product", [["Features", "#features"], ["How it works", "#install"], ["From Vercel", "#vercel"], ["FAQ", "#faq"]]],
  [
    "Docs",
    [
      ["Setup guide", DOCS],
      ["vercel.json support", `${REPO}/blob/main/docs/PROJECT-CONFIG.md`],
      ["Vercel parity", `${REPO}/blob/main/docs/VERCEL-PARITY.md`],
      ["Troubleshooting", `${REPO}/blob/main/docs/TROUBLESHOOTING.md`],
    ],
  ],
  [
    "Project",
    [
      ["GitHub", REPO],
      ["Contributing", `${REPO}/blob/main/CONTRIBUTING.md`],
      ["Security", `${REPO}/blob/main/SECURITY.md`],
      ["Changelog", `${REPO}/blob/main/CHANGELOG.md`],
    ],
  ],
] as const;

/** Footer links. Sits inside the beam's track, so the rail runs past it. */
export function Footer() {
  return (
    <Section junction={false} className="pb-10">
      <footer className="border-t border-line pt-10 sm:pt-14">
        <div className="grid grid-cols-2 gap-x-6 gap-y-10 sm:grid-cols-3 sm:gap-12 md:grid-cols-[1.5fr_1fr_1fr_1fr]">
          <div className="col-span-2 sm:col-span-3 md:col-span-1">
            <Logo />
            <p className="serif mt-5 text-[30px] leading-none text-fg">Free, forever.</p>
            <p className="mt-3 max-w-[300px] text-[14px] leading-relaxed text-fg-dim">
              Your own Vercel on an old laptop. Open source, built in the open.
            </p>
            <div className="mt-6 flex flex-wrap items-center gap-2">
              <span className="inline-flex items-center gap-2 rounded-full border border-line px-3 py-1.5 font-mono text-[11px] text-fg-muted">
                <Dot tone="ok" pulse /> MIT licensed · v0.2.0
              </span>
              <a
                href={REPO}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-2 rounded-full border border-line px-3 py-1.5 font-mono text-[11px] text-fg-muted transition-colors hover:border-line-strong hover:text-fg"
              >
                <GithubIcon className="size-3" /> shashank03-dev/vercelify-oss
              </a>
            </div>
          </div>
          {FOOTER.map(([title, links], n) => (
            <nav key={title} aria-label={title}>
              <div className="flex items-baseline gap-2 font-mono text-[11px] uppercase tracking-[0.1em]">
                <span className="text-accent/80">0{n + 1}</span>
                <span className="text-fg-dim">{title}</span>
              </div>
              <ul className="mt-5 space-y-3">
                {links.map(([label, href]) => (
                  <li key={label}>
                    <a
                      href={href}
                      {...(href.startsWith("http") ? { target: "_blank", rel: "noreferrer" } : {})}
                      className="group inline-flex items-center gap-1.5 py-0.5 text-[14.5px] text-fg-muted transition-colors hover:text-fg"
                    >
                      {label}
                      {href.startsWith("http") && (
                        <span className="text-fg-dim opacity-0 transition-opacity group-hover:opacity-100">↗</span>
                      )}
                    </a>
                  </li>
                ))}
              </ul>
            </nav>
          ))}
        </div>
        <div className="mt-12 flex flex-wrap justify-between gap-3 font-mono text-[11px] text-fg-dim sm:mt-16">
          <span>© 2026 Vercelify contributors</span>
          <span>Built on Coolify, Tailscale and Cloudflare · Not affiliated with Vercel Inc.</span>
        </div>
      </footer>
    </Section>
  );
}

/**
 * Where the beam lands for the last time: a horizon across the page, a pool
 * of light, and the wordmark rising out of it. The letters pick up the logo's
 * chevron glyphs only where the light reaches them.
 */
/**
 * The end of the pour. The beam arrives at the horizon on the rail's line,
 * runs along it both ways, and the giant wordmark under it stays dark until
 * the light gets there: it surfaces as the reader reaches the bottom, lit
 * from the landing point outward.
 */
export function Horizon() {
  const ref = useRef<HTMLDivElement>(null);
  const landRef = useRef<HTMLSpanElement>(null);
  const webgl = useShader();
  const near = useInView(ref, { once: true, amount: 0.35 });
  const { scrollYProgress } = useScroll({ target: ref, offset: ["start end", "end end"] });
  const wm = useTransform(scrollYProgress, [0.3, 1], [0, 1], { clamp: true });
  const base = useTransform(scrollYProgress, [0.1, 0.85], [0, 1], { clamp: true });
  const lit = !!webgl;

  return (
    <div
      aria-hidden
      ref={ref}
      className="relative"
      style={{ ["--land" as string]: "calc(max((100% - 1200px) / 2, 0px) + var(--pad))" }}
    >
      {lit && near && (
        <LaserLanding
          surfaceRef={ref}
          beamRef={landRef}
          className="inset-x-0 top-[-340px]"
          below={140}
          fadeTop
          intensity={0.62}
          spread={3.4}
          spill={0}
          fog={0.12}
        />
      )}
      <span ref={landRef} className="absolute top-0 w-0" style={{ left: "var(--land)" }} />
      <div className="relative h-[clamp(110px,18vw,270px)] overflow-hidden">
        <m.div
          className="absolute inset-0"
          initial={{ opacity: 0.35 }}
          whileInView={{ opacity: 1 }}
          viewport={{ once: false, amount: 0.6 }}
          transition={{ duration: 1.2 }}
        >
          <div className={cx("horizon-pool", lit && "horizon-pool--soft")} />
          <div className="horizon-line" />
        </m.div>
        <div className="absolute top-[14%]" style={{ left: "var(--land)" }}>
          <m.div
            className="relative text-[min(19vw,300px)] [--land-in:3%]"
            style={{ ["--wm" as string]: wm, ["--wm-base" as string]: base }}
          >
            <span className="wordmark wordmark--base block">Vercelify</span>
            <span className="wordmark wordmark--lit absolute inset-0 block">Vercelify</span>
          </m.div>
        </div>
      </div>
    </div>
  );
}
