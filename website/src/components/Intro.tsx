import {
  type MotionValue,
  m,
  useScroll,
  useTransform,
} from "framer-motion";
import { useRef } from "react";
import { LaserLanding, useMedia, useShader } from "./LaserLanding";
import { Reveal, Section, cx } from "./ui";

const STACK = [
  "Coolify",
  "Tailscale",
  "Cloudflare Tunnel",
  "Traefik",
  "Docker",
  "Let's Encrypt",
  "Nixpacks",
  "Rust",
  "Tauri",
];

const FACTS = [
  ["$0", "a month, forever"],
  ["8 GB", "of RAM runs several apps"],
  ["3 MB", "host agent, written in Rust"],
  ["~30 min", "from blank laptop to live URL"],
] as const;

/**
 * Trust block: what it is built on, and the numbers that matter. The rail's
 * light lands on the numbers card the same way it lands on the app window in
 * the hero: on its top-left corner, running along the top edge and pouring
 * down the side back into the rail. The card reaches left past the rail so
 * the beam meets it exactly on the rail's line.
 */
export function Stack() {
  const surfaceRef = useRef<HTMLDivElement>(null);
  const webgl = useShader();
  const wide = useMedia("(min-width: 768px)");
  const lit = !!webgl && wide;
  return (
    <Section junction={!lit} className="pt-12 pb-6 sm:pt-16">
      {lit && (
        <LaserLanding
          surfaceRef={surfaceRef}
          fadeTop
          intensity={0.7}
          className="top-0 right-0 left-[calc(-1*var(--indent)-160px)]"
          below={360}
          spread={2.4}
          spill={0.45}
        />
      )}
      <Reveal>
        <p className="font-mono text-[11px] uppercase tracking-[0.1em] text-fg-dim">
          Built on open-source tools you already trust
        </p>
        <div className="marquee mt-6 overflow-hidden">
          <div className="marquee-track">
            {[...STACK, ...STACK].map((name, i) => (
              <span
                key={i}
                className="flex items-center gap-3 px-7 text-[17px] font-medium tracking-tight whitespace-nowrap text-fg-muted/80"
              >
                <span className="size-1 rounded-full bg-fg-dim" />
                {name}
              </span>
            ))}
          </div>
        </div>
      </Reveal>
      {/* LaserFlow lights a surface's bottom edge too. The beam lands on this
          taller, invisible twin of the card, so the light only runs along the
          card's top and pours down its left side. */}
      <div
        className={cx(
          "relative mt-14 sm:mt-20",
          lit && "ml-[calc(-1*var(--indent)-22px)]",
        )}
      >
        {lit && (
          <div
            ref={surfaceRef}
            aria-hidden
            className="pointer-events-none absolute inset-x-0 top-0 h-[1000px] rounded-2xl"
          />
        )}
        <div
          className={cx(
            "relative grid grid-cols-2 overflow-hidden rounded-2xl border border-line-strong bg-panel md:grid-cols-4",
          )}
        >
          {FACTS.map(([n, label], i) => (
            <div
              key={n}
              className={cx(
                "px-4 py-6 sm:px-6 sm:py-8",
                i % 2 === 1 && "border-l border-line",
                i > 1 && "border-t border-line md:border-t-0",
                i === 2 && "md:border-l",
                lit && i === 0 && "pl-[calc(var(--indent)+22px)]",
              )}
            >
              <div
                className={cx(
                  "text-[clamp(28px,3.4vw,40px)] font-medium tracking-[-0.03em]",
                  i === 0 && "text-brand",
                )}
              >
                {n}
              </div>
              <div className="mt-1 text-[14px] text-fg-muted">{label}</div>
            </div>
          ))}
        </div>
      </div>
    </Section>
  );
}

const STATEMENT =
  "Your old laptop already has the CPU, memory and disk. Vercelify adds the builds, routing, certificates and a Mac app to run it all.";

/** A statement that lights up word by word as the beam passes it. */
export function Statement() {
  const ref = useRef<HTMLParagraphElement>(null);
  const { scrollYProgress } = useScroll({
    target: ref,
    offset: ["start 0.85", "end 0.45"],
  });
  const words = STATEMENT.split(" ");
  return (
    <Section className="py-24 sm:py-36">
      <p
        ref={ref}
        className="max-w-[980px] text-[clamp(28px,4.4vw,56px)] leading-[1.12] font-medium tracking-[-0.03em] text-balance"
      >
        {words.map((w, n) => (
          <Word
            key={n}
            word={w}
            progress={scrollYProgress}
            range={[n / words.length, (n + 1) / words.length]}
          />
        ))}
      </p>
    </Section>
  );
}

function Word({
  word,
  progress,
  range,
}: {
  word: string;
  progress: MotionValue<number>;
  range: [number, number];
}) {
  const opacity = useTransform(progress, range, [0.16, 1]);
  return (
    <m.span style={{ opacity }} className="text-fg">
      {word}{" "}
    </m.span>
  );
}
