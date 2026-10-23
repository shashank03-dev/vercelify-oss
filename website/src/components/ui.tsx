import { m } from "framer-motion";
import { Check, Copy } from "lucide-react";
import { type ImgHTMLAttributes, type KeyboardEvent, type ReactNode, useRef, useState } from "react";
import SpecularButton from "./SpecularButton/SpecularButton.jsx";

export const REPO = "https://github.com/shashank03-dev/vercelify-oss";
export const DOCS = `${REPO}#readme`;
export const INSTALL_HOST = "curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash";
export const INSTALL_MAC = "curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install-mac.sh | bash";

/**
 * Two cores, under 4 GB of memory or data-saver on: skip the GPU effects and
 * keep only the CSS ones.
 */
export function weakDevice() {
  const nav = navigator as Navigator & { deviceMemory?: number; connection?: { saveData?: boolean } };
  return (nav.hardwareConcurrency || 8) <= 2 || (nav.deviceMemory ?? 8) < 4 || !!nav.connection?.saveData;
}

export function cx(...c: (string | false | null | undefined)[]) {
  return c.filter(Boolean).join(" ");
}

export function Mark({ className = "h-5" }: { className?: string }) {
  return <img src="/mark.svg" alt="" className={className} draggable={false} />;
}

export function Logo() {
  return (
    <span className="flex items-center gap-2.5">
      <Mark className="h-[18px]" />
      <span className="text-[15px] font-medium tracking-tight">Vercelify</span>
    </span>
  );
}

/** Mono uppercase chip with a square bullet, as in the reference's "■ WHAT WE DO". */
export function Eyebrow({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={cx(
        "inline-flex items-center gap-2 rounded-md border border-line-strong bg-surface/70 px-2.5 py-1 font-mono text-[11px] uppercase tracking-[0.08em] text-fg-muted",
        className,
      )}
    >
      <span className="size-1.5 rounded-[1px] bg-accent" />
      {children}
    </span>
  );
}

/** Heading in two tones: a statement, then a muted line that earns it. */
export function SectionHeader({
  eyebrow,
  title,
  muted,
  align = "left",
  size = "lg",
  className,
}: {
  eyebrow: string;
  title: ReactNode;
  muted?: ReactNode;
  align?: "left" | "center";
  size?: "lg" | "md";
  className?: string;
}) {
  return (
    <Reveal className={cx(align === "center" && "text-center", className)}>
      <Eyebrow>{eyebrow}</Eyebrow>
      <h2
        className={cx(
          "mt-5 leading-[1.08] font-medium tracking-[-0.025em] text-balance",
          size === "lg" ? "text-[clamp(28px,4vw,44px)]" : "text-[clamp(26px,3vw,36px)]",
          align === "center" ? "mx-auto max-w-3xl" : "max-w-3xl",
        )}
      >
        <span className="text-fg">{title}</span> {muted && <span className="text-fg-dim">{muted}</span>}
      </h2>
    </Reveal>
  );
}

export function Reveal({ children, className, delay = 0 }: { children: ReactNode; className?: string; delay?: number }) {
  return (
    <m.div
      className={className}
      initial={{ opacity: 0, y: 14 }}
      whileInView={{ opacity: 1, y: 0 }}
      viewport={{ once: true, margin: "-60px" }}
      transition={{ duration: 0.6, ease: [0.22, 1, 0.36, 1], delay }}
    >
      {children}
    </m.div>
  );
}

export function Button({
  href,
  children,
  variant = "primary",
  className,
}: {
  href: string;
  children: ReactNode;
  variant?: "primary" | "ghost";
  className?: string;
}) {
  const external = href.startsWith("http");
  return (
    <a
      href={href}
      {...(external ? { target: "_blank", rel: "noreferrer" } : {})}
      className={cx(
        "inline-flex h-10 items-center gap-2 rounded-lg px-4 font-mono text-[12px] font-medium uppercase tracking-[0.06em] transition-[background,box-shadow,color] duration-200",
        variant === "primary"
          ? "bg-[#f4f1ec] text-[#141312] shadow-[0_0_0_1px_rgb(255_255_255/0.6)_inset,0_8px_30px_-8px_rgb(255_138_74/0.45)] hover:bg-white"
          : "border border-line-strong bg-surface/80 text-fg hover:bg-surface-3",
        className,
      )}
    >
      {children}
    </a>
  );
}

function legacyCopy(text: string) {
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.cssText = "position:fixed;top:0;left:0;opacity:0";
  document.body.appendChild(area);
  area.select();
  try {
    document.execCommand("copy");
  } catch {
    // Nothing else to try; the command is still on screen to copy by hand.
  }
  area.remove();
}

/**
 * Arrow/Home/End keys for a row of tabs (WAI-ARIA tabs pattern): moves focus
 * and selects in one go. Put it on the tablist's onKeyDown.
 */
export function tabKeys(count: number, current: number, select: (n: number) => void) {
  return (e: KeyboardEvent<HTMLElement>) => {
    const next =
      e.key === "ArrowRight" ? (current + 1) % count
      : e.key === "ArrowLeft" ? (current - 1 + count) % count
      : e.key === "Home" ? 0
      : e.key === "End" ? count - 1
      : -1;
    if (next < 0) return;
    e.preventDefault();
    select(next);
    e.currentTarget.querySelectorAll<HTMLElement>('[role="tab"]')[next]?.focus();
  };
}

export function useCopy() {
  const [copied, setCopied] = useState(false);
  const t = useRef<number | undefined>(undefined);
  return {
    copied,
    copy(text: string) {
      // The async clipboard is missing on plain-http hosts and older Safari/Firefox.
      if (navigator.clipboard?.writeText) void navigator.clipboard.writeText(text).catch(() => legacyCopy(text));
      else legacyCopy(text);
      setCopied(true);
      window.clearTimeout(t.current);
      t.current = window.setTimeout(() => setCopied(false), 1600);
    },
  };
}

/**
 * A shell line that copies itself when clicked. Shows `display`, copies
 * `command`. The edge is React Bits' SpecularButton: a hairline that catches
 * an ember light on the side nearest the cursor.
 */
export function CopyCommand({ command, display, className }: { command: string; display?: string; className?: string }) {
  const { copied, copy } = useCopy();
  return (
    <SpecularButton
      size="sm"
      radius={10}
      tint="#080807"
      tintOpacity={0.85}
      blur={0}
      textColor="var(--fg)"
      lineColor="#ffc9a8"
      baseColor="#3a3733"
      intensity={0.9}
      idleIntensity={0.3}
      shineSize={12}
      shineFade={40}
      thickness={1.1}
      glow={0.8}
      proximity={220}
      onClick={() => copy(command)}
      aria-label={copied ? "Copied" : `Copy command: ${command}`}
      className={cx(
        "group flex w-full justify-start py-2 pr-2 pl-3.5 font-mono text-[12.5px] font-normal tracking-normal bg-[linear-gradient(180deg,rgb(255_255_255/0.06),rgb(255_255_255/0)_45%),rgb(8_8_7/0.84)] shadow-[inset_0_1px_0_rgb(255_255_255/0.07),inset_0_0_0_1px_rgb(255_255_255/0.06),0_14px_34px_-14px_rgb(0_0_0/0.9)] [&>span]:w-full [&>span]:min-w-0 [&>span]:gap-3",
        className,
      )}
    >
      <span className="font-medium text-accent select-none">$</span>
      <code className="min-w-0 flex-1 truncate text-left leading-7 text-fg">{display ?? command}</code>
      <span className="grid size-7 shrink-0 place-items-center rounded-md text-fg-muted transition-colors group-hover:bg-surface-3 group-hover:text-fg">
        {copied ? <Check className="size-3.5 text-ok" /> : <Copy className="size-3.5" />}
      </span>
      <span role="status" className="sr-only">
        {copied ? "Copied to clipboard" : ""}
      </span>
    </SpecularButton>
  );
}

export function Dot({ tone = "ok", pulse }: { tone?: "ok" | "warn" | "bad" | "accent" | "dim"; pulse?: boolean }) {
  const colors = { ok: "bg-ok text-ok", warn: "bg-warn text-warn", bad: "bg-bad text-bad", accent: "bg-accent text-accent", dim: "bg-fg-dim text-fg-dim" };
  return <span className={cx("inline-block size-1.5 shrink-0 rounded-full", colors[tone], pulse && "pulse-dot")} />;
}

/** Status is always dot + label. */
export function Status({ tone = "ok", children }: { tone?: "ok" | "warn" | "bad" | "accent"; children: ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-line px-2 py-0.5 text-[11px] text-fg-muted">
      <Dot tone={tone} />
      {children}
    </span>
  );
}

/** Card with the pointer spotlight (React Bits' Spotlight Card). */
export function Card({ children, className }: { children: ReactNode; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={ref}
      onPointerMove={(e) => {
        if (e.pointerType !== "mouse") return;
        const r = ref.current!.getBoundingClientRect();
        ref.current!.style.setProperty("--mx", `${e.clientX - r.left}px`);
        ref.current!.style.setProperty("--my", `${e.clientY - r.top}px`);
      }}
      className={cx(
        "spotlight relative overflow-hidden rounded-2xl border border-line bg-[linear-gradient(180deg,var(--surface),var(--panel))] shadow-[0_1px_0_0_rgb(255_255_255/0.03)_inset]",
        className,
      )}
    >
      {children}
    </div>
  );
}

/**
 * Page section. Content sits --indent right of the page rail (the beam's
 * line); `junction` draws the small connector that plugs it into the rail.
 */
export function Section({
  id,
  children,
  className,
  junction = true,
}: {
  id?: string;
  children: ReactNode;
  className?: string;
  junction?: boolean;
}) {
  return (
    <section id={id} className={cx("relative mx-auto w-full max-w-[1200px] px-4 sm:px-6 lg:px-8", className)}>
      <div className="relative pl-[var(--indent)]">
        {junction && <Junction />}
        {children}
      </div>
    </section>
  );
}

/** A node on the rail and a tick running right to the section's content. */
export function Junction({ top = 12 }: { top?: number }) {
  return (
    <div aria-hidden className="pointer-events-none absolute left-0" style={{ top }}>
      <m.span
        className="absolute top-0 left-0 h-px w-[calc(var(--indent)-10px)] origin-left bg-[linear-gradient(90deg,var(--ember-2),color-mix(in_srgb,var(--ember-1)_30%,transparent))]"
        initial={{ scaleX: 0 }}
        whileInView={{ scaleX: 1 }}
        viewport={{ once: true, margin: "-35% 0px -35% 0px" }}
        transition={{ duration: 0.7, ease: [0.22, 1, 0.36, 1] }}
      />
      <m.span
        className="absolute -top-[3.5px] -left-[3.5px] size-2 rotate-45 border border-accent/70 bg-bg"
        initial={{ backgroundColor: "var(--bg)", boxShadow: "0 0 0 0 transparent" }}
        whileInView={{ backgroundColor: "var(--accent)", boxShadow: "0 0 12px 2px rgb(255 106 61 / 0.55)" }}
        viewport={{ once: true, margin: "-35% 0px -35% 0px" }}
        transition={{ duration: 0.4 }}
      />
    </div>
  );
}

/**
 * A 2720px app capture (`src` is the full-size .jpg in public/app). Served as
 * AVIF, with 1360px copies for 1x screens and phones; JPEG only as fallback.
 */
export function Screenshot({ src, sizes, onError, ...img }: { src: string; sizes: string } & ImgHTMLAttributes<HTMLImageElement>) {
  const base = src.replace(/\.jpg$/, "");
  // A browser can claim AVIF and still fail to decode a file (Firefox and
  // grid AVIFs): drop the AVIF source so the <img> falls back to the JPEG.
  const [avif, setAvif] = useState(true);
  return (
    <picture className="contents">
      {avif && <source type="image/avif" srcSet={`${base}-1360.avif 1360w, ${base}.avif 2720w`} sizes={sizes} />}
      <img
        onError={(e) => {
          if (avif && e.currentTarget.currentSrc.endsWith(".avif")) setAvif(false);
          onError?.(e);
        }}
        src={src}
        srcSet={`${base}-1360.jpg 1360w, ${src} 2720w`}
        sizes={sizes}
        width={2720}
        height={1720}
        decoding="async"
        draggable={false}
        {...img}
      />
    </picture>
  );
}

/** A real screen from the app (captured from the cockpit's demo mode), cropped with object-position. */
export function Shot({
  src,
  alt,
  pos = "0% 0%",
  zoom = 1,
  width = "(min-width: 1024px) 560px, 100vw",
  className,
  imgClassName,
  eager,
}: {
  src: string;
  alt: string;
  /** object-position, and the point the zoom scales around. */
  pos?: string;
  zoom?: number;
  /** Rendered width as a `sizes` value, before the zoom. */
  width?: string;
  className?: string;
  imgClassName?: string;
  eager?: boolean;
}) {
  // The zoom crops in, so the image needs zoom× the pixels of its frame.
  const sizes = width.replace(/(\d+(?:px|vw))(?=,|$)/g, (w) => `calc(${zoom} * ${w})`);
  return (
    <div className={cx("relative overflow-hidden rounded-xl border border-line-strong bg-[#121212]", className)}>
      <Screenshot
        src={src}
        alt={alt}
        sizes={sizes}
        loading={eager ? "eager" : "lazy"}
        className={cx("size-full object-cover", imgClassName)}
        style={{ objectPosition: pos, transform: zoom === 1 ? undefined : `scale(${zoom})`, transformOrigin: pos }}
      />
    </div>
  );
}

export function GithubIcon({ className = "size-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" aria-hidden className={className}>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" />
    </svg>
  );
}
