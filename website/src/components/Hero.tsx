import { motion, useScroll } from "framer-motion";
import { ArrowRight, Menu, X } from "lucide-react";
import { type RefObject, useEffect, useRef, useState } from "react";
import { LaserLanding, useWebgl2 } from "./LaserLanding";
import { HeroShards } from "./HeroShards";
import { Button, CopyCommand, GithubIcon as Github, INSTALL_HOST, Logo, REPO, cx } from "./ui";

const LINKS = [
  ["Features", "#features"],
  ["How it works", "#install"],
  ["From Vercel", "#vercel"],
  ["FAQ", "#faq"],
] as const;

/**
 * At the top the nav is a plain full-width bar. Once the page scrolls it
 * draws in to a notch hanging from the top edge (concave "ears" join it to
 * the edge), with the reading progress as an ember line along its bottom.
 * Width, height, radius and colour all ease with one CSS transition.
 */
export function Nav() {
  const [scrolled, setScrolled] = useState(false);
  const [open, setOpen] = useState(false);
  const { scrollYProgress } = useScroll();
  useEffect(() => {
    const on = () => setScrolled(window.scrollY > 24);
    on();
    window.addEventListener("scroll", on, { passive: true });
    return () => window.removeEventListener("scroll", on);
  }, []);
  const notch = scrolled || open;

  return (
    <header className="pointer-events-none fixed inset-x-0 top-0 z-50 flex justify-center">
      <div
        className={cx(
          "notch pointer-events-auto relative w-full transition-[max-width,background-color,border-color,border-radius,box-shadow] duration-500 ease-[cubic-bezier(0.22,1,0.36,1)]",
          notch
            ? "notch--on max-w-[calc(100%-24px)] rounded-b-[20px] border border-t-0 border-line-strong bg-[#0d0d0c]/92 shadow-[0_18px_40px_-18px_rgb(0_0_0/0.8)] backdrop-blur-md sm:max-w-[780px]"
            : "max-w-[1200px] rounded-b-none border border-t-0 border-transparent",
        )}
      >
        <nav
          className={cx(
            "flex items-center justify-between transition-[height,padding] duration-500 ease-[cubic-bezier(0.22,1,0.36,1)]",
            notch ? "h-14 px-3 sm:pr-2.5 sm:pl-4" : "h-16 px-4 sm:px-6 lg:px-8",
          )}
        >
          <a href="#top" aria-label="Vercelify home">
            <Logo />
          </a>
          <div className={cx("hidden items-center transition-[gap] duration-500 md:flex", notch ? "gap-6" : "gap-8")}>
            {LINKS.map(([label, href]) => (
              <a
                key={href}
                href={href}
                className="font-mono text-[11.5px] uppercase tracking-[0.08em] text-fg-muted transition-colors hover:text-fg"
              >
                {label}
              </a>
            ))}
          </div>
          <div className="flex items-center gap-2">
            <a
              href={REPO}
              target="_blank"
              rel="noreferrer"
              className="hidden h-9 items-center gap-2 rounded-lg border border-line-strong bg-surface/70 px-3 font-mono text-[11.5px] uppercase tracking-[0.06em] text-fg transition-colors hover:bg-surface-3 sm:flex"
            >
              <Github className="size-3.5" /> GitHub
            </a>
            <button
              type="button"
              className="grid size-9 place-items-center rounded-lg text-fg-muted md:hidden"
              aria-label={open ? "Close menu" : "Open menu"}
              aria-expanded={open}
              onClick={() => setOpen(!open)}
            >
              {open ? <X className="size-5" /> : <Menu className="size-5" />}
            </button>
          </div>
        </nav>
        {open && (
          <div className="border-t border-line px-4 pt-2 pb-4 md:hidden">
            {[...LINKS, ["GitHub", REPO] as const].map(([label, href]) => (
              <a
                key={href}
                href={href}
                onClick={() => setOpen(false)}
                className="block py-2.5 font-mono text-[12px] uppercase tracking-[0.08em] text-fg-muted"
              >
                {label}
              </a>
            ))}
          </div>
        )}
        <motion.div
          aria-hidden
          className={cx(
            "absolute inset-x-5 -bottom-px h-px origin-left bg-gradient-to-r from-ember-1 via-accent to-[#ffd2b8] transition-opacity duration-500",
            notch ? "opacity-100" : "opacity-0",
          )}
          style={{ scaleX: scrollYProgress }}
        />
      </div>
    </header>
  );
}

const rise = (delay: number) => ({
  initial: { opacity: 0, y: 18, filter: "blur(6px)" },
  animate: { opacity: 1, y: 0, filter: "blur(0px)" },
  transition: { duration: 0.8, ease: [0.22, 1, 0.36, 1] as const, delay },
});

/** Real screens, captured from the cockpit's demo mode (VITE_DEMO=1). */
const SCREENS = [
  { id: "apps", label: "Apps", alt: "Vercelify Apps page: six apps with status, deploy buttons and server health" },
  { id: "deployments", label: "Deployments", alt: "Deployment history of an app with previews, an error and rollback buttons" },
  { id: "health", label: "Health", alt: "Health page: CPU, memory, disk, temperature and the power and thermal guard" },
  { id: "palette", label: "⌘K", alt: "Command palette searching for deploy" },
] as const;

export function Hero() {
  const windowRef = useRef<HTMLDivElement>(null);
  const webgl = useWebgl2();
  return (
    <section id="top" className="relative overflow-hidden pt-16">
      <div aria-hidden className="pointer-events-none hero-bg absolute inset-0">
        <div className="glyph-field glyph-field--dim" />
        <div className="glyph-field glyph-field--hot" />
        <HeroShards />
        {webgl && <LaserLanding surfaceRef={windowRef} className="inset-x-0 top-0" revealImage="/app/apps.jpg" />}
        <div className="absolute inset-x-0 top-0 h-[560px] bg-[radial-gradient(ellipse_55%_80%_at_75%_0%,color-mix(in_srgb,var(--cool)_11%,transparent),transparent_70%)]" />
      </div>

      <div className="relative mx-auto max-w-[1200px] px-4 sm:px-6 lg:px-8">
        <div className="relative pl-[var(--indent)] pt-12 sm:pt-16">
          <motion.a
            {...rise(0)}
            href={REPO}
            target="_blank"
            rel="noreferrer"
            className="group inline-flex items-center gap-2 rounded-md border border-line-strong bg-surface/70 py-1 pr-2 pl-2.5 font-mono text-[11px] uppercase tracking-[0.08em] backdrop-blur"
          >
            <span className="size-1.5 rounded-[1px] bg-accent" />
            <span className="shiny">Now open source · MIT licensed</span>
            <ArrowRight className="size-3 text-fg-dim transition-transform group-hover:translate-x-0.5" />
          </motion.a>

          <motion.h1
            {...rise(0.08)}
            className="mt-6 max-w-[18ch] text-[clamp(40px,6.2vw,84px)] leading-[0.96] font-medium tracking-[-0.045em]"
          >
            <span className="text-headline">Your own </span>
            <span className="serif text-brand pr-1 text-[1.1em]">Vercel,</span>
            <br className="hidden sm:block" />
            <span className="text-headline"> on the </span>
            <span className="serif text-headline text-[1.1em]">laptop</span>
            <span className="text-headline"> in your drawer.</span>
          </motion.h1>

          <motion.p {...rise(0.18)} className="mt-6 max-w-[600px] text-[17px] leading-[1.6] text-fg-muted sm:text-[18px]">
            Vercelify turns a spare PC, old laptop or Raspberry Pi into a hosting platform: push-to-deploy, pull request
            previews, rollbacks, databases and HTTPS on your own domain. Free, forever.
          </motion.p>

          <motion.div {...rise(0.26)} className="mt-8 flex flex-wrap items-center gap-3">
            <Button href="#install">
              Install on your laptop <ArrowRight className="size-3.5" />
            </Button>
            <Button href={REPO} variant="ghost">
              <Github className="size-3.5" /> Star on GitHub
            </Button>
          </motion.div>

          <motion.div {...rise(0.34)} className="mt-5 max-w-[520px]">
            <CopyCommand command={INSTALL_HOST} display="curl -fsSL …/vercelify-oss/main/install.sh | sudo bash" />
            <p className="mt-2.5 pl-1 font-mono text-[11px] text-fg-muted">Ubuntu 24.04 · Debian 12 · Raspberry Pi OS 64-bit</p>
          </motion.div>
        </div>

        <Showcase windowRef={windowRef} pour={webgl === false} />
      </div>
    </section>
  );
}

/**
 * The app window, lit by the pour. The beam lands on its rounded top-left
 * corner, the light runs along the top edge and down the left side (where
 * the page rail picks it up) and spills a little into the window.
 */
function Showcase({ windowRef, pour }: { windowRef: RefObject<HTMLDivElement | null>; pour: boolean }) {
  const [i, setI] = useState(0);
  const [auto, setAuto] = useState(true);
  useEffect(() => {
    if (!auto) return;
    const t = window.setInterval(() => setI((n) => (n + 1) % SCREENS.length), 5200);
    return () => window.clearInterval(t);
  }, [auto]);

  return (
    <motion.div
      initial={{ opacity: 0, y: 40 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 1.1, ease: [0.22, 1, 0.36, 1], delay: 0.45 }}
      className="relative mt-14 sm:mt-16"
    >
      {pour && (
        <>
          <div aria-hidden className="pour">
            <div className="pour-haze" />
            <div className="pour-glow" />
            <div className="pour-streaks" />
            <div className="pour-grain" />
            <div className="pour-core" />
          </div>
          <div aria-hidden className="bloom" />
        </>
      )}

      <div
        ref={windowRef}
        className="relative overflow-hidden rounded-t-[14px] rounded-b-none border border-b-0 border-line-strong bg-[#0d0d0c] shadow-[0_40px_120px_-30px_rgb(0_0_0/0.9)]">
        {pour && <div aria-hidden className="spill" />}
        <div className="flex h-11 items-center gap-3 border-b border-line px-4">
          <div className="flex gap-1.5">
            <span className="size-3 rounded-full bg-surface-3" />
            <span className="size-3 rounded-full bg-surface-3" />
            <span className="size-3 rounded-full bg-surface-3" />
          </div>
          <div role="tablist" aria-label="App screens" className="mx-auto flex gap-1 rounded-lg border border-line bg-surface/60 p-0.5">
            {SCREENS.map((s, n) => (
              <button
                key={s.id}
                role="tab"
                type="button"
                aria-selected={i === n}
                onClick={() => {
                  setI(n);
                  setAuto(false);
                }}
                className={cx(
                  "relative rounded-md px-2.5 py-1 font-mono text-[11px] tracking-[0.02em] transition-colors sm:px-3",
                  i === n ? "bg-surface-3 text-fg" : "text-fg-dim hover:text-fg-muted",
                )}
              >
                {s.label}
              </button>
            ))}
          </div>
          <span className="hidden w-[52px] sm:block" />
        </div>
        <div className="relative aspect-[2720/1640]">
          {SCREENS.map((s, n) => (
            <img
              key={s.id}
              src={`/app/${s.id}.jpg`}
              alt={s.alt}
              width={2720}
              height={1720}
              loading={n === 0 ? "eager" : "lazy"}
              decoding="async"
              draggable={false}
              className={cx(
                "absolute inset-0 size-full object-cover object-top transition-opacity duration-500",
                i === n ? "opacity-100" : "opacity-0",
              )}
            />
          ))}
          {/* Sink the bottom of the window into the page. */}
          <div className="absolute inset-x-0 bottom-0 h-1/3 bg-gradient-to-b from-transparent to-bg" />
        </div>
      </div>
      <div aria-hidden className={cx("rim-left", !pour && "rim-left--flow")} />
      {pour && (
        <>
          <div aria-hidden className="rim-top" />
          <div aria-hidden className="rim-corner" />
          <div aria-hidden className="impact" />
        </>
      )}
    </motion.div>
  );
}
