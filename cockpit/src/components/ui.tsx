import { animate, AnimatePresence, motion } from "framer-motion";
import { Check, ChevronDown, Copy, Loader2, X } from "lucide-react";
import {
  cloneElement,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type PointerEvent as ReactPointerEvent,
  type ReactElement,
  type ReactNode,
  type SelectHTMLAttributes,
} from "react";
import { createPortal } from "react-dom";
import type { Health } from "../lib/format";

export function cx(...c: (string | false | null | undefined)[]) {
  return c.filter(Boolean).join(" ");
}

// --- brand -------------------------------------------------------------------

/** The Vercelify mark (docs/brand/vercelify-mark.svg). */
export function Logo({ className, mono }: { className?: string; mono?: boolean }) {
  const id = useId();
  const fill = mono ? "currentColor" : `url(#${id}-g)`;
  return (
    <svg viewBox="198 260 628 466" className={className} aria-hidden="true">
      <defs>
        <linearGradient id={`${id}-g`} gradientUnits="userSpaceOnUse" x1="400" y1="760" x2="620" y2="250">
          <stop offset="0" stopColor="#FF4A2B" />
          <stop offset="0.55" stopColor="#FF8A4A" />
          <stop offset="1" stopColor="#FFD596" />
        </linearGradient>
        <mask id={`${id}-m`}>
          <rect x="0" y="0" width="1024" height="1024" fill="#fff" />
          <rect x="406.4" y="655.4" width="211.3" height="31.9" rx="15.9" fill="#000" />
        </mask>
      </defs>
      <g mask={`url(#${id}-m)`} fill={fill} stroke={fill} strokeWidth="52" strokeLinejoin="round">
        <path d="M512 290.2 L750.2 478.1 L750.2 546.4 L731.4 547.1 L512 377.7 L292.6 547.1 L273.8 546.4 L273.8 478.1 Z" />
        <path d="M293.9 613.5 L730.1 613.5 L795.5 686.4 L788.8 695.6 L235.2 695.6 L228.5 686.4 Z" />
      </g>
    </svg>
  );
}

// --- surfaces ------------------------------------------------------------------

/** Pointer-tracking glow; only writes two CSS variables per move. */
function useSpotlight() {
  return useCallback((e: ReactPointerEvent<HTMLElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    e.currentTarget.style.setProperty("--mx", `${e.clientX - r.left}px`);
    e.currentTarget.style.setProperty("--my", `${e.clientY - r.top}px`);
  }, []);
}

export function Card({
  children,
  className,
  title,
  description,
  icon,
  actions,
  testId,
  flush,
  spotlight,
}: {
  children: ReactNode;
  className?: string;
  title?: ReactNode;
  description?: ReactNode;
  icon?: ReactNode;
  actions?: ReactNode;
  testId?: string;
  /** No body padding (tables and lists that run edge to edge). */
  flush?: boolean;
  spotlight?: boolean;
}) {
  const onMove = useSpotlight();
  return (
    <section
      className={cx("rounded-xl border border-line bg-surface", spotlight && "spotlight", className)}
      data-testid={testId}
      onPointerMove={spotlight ? onMove : undefined}
    >
      {(title || actions) && (
        <header className="flex min-h-11 flex-wrap items-center justify-between gap-x-3 gap-y-2 border-b border-line px-4 py-2">
          <div className="min-w-0 flex-[1_1_12rem]">
            <h2 className="flex items-center gap-2 text-[13px] font-medium text-fg">
              {icon && <span className="text-fg-dim [&>svg]:size-4">{icon}</span>}
              {title}
            </h2>
            {description && <p className="mt-0.5 text-xs text-fg-dim">{description}</p>}
          </div>
          {actions && <div className="flex shrink-0 items-center gap-1.5">{actions}</div>}
        </header>
      )}
      <div className={flush ? undefined : "p-4"}>{children}</div>
    </section>
  );
}

export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="mb-6 flex flex-wrap items-center justify-between gap-4">
      <div className="min-w-0">
        <h1 className="text-[20px] leading-7 font-semibold tracking-[-0.02em]">{title}</h1>
        {subtitle && <p className="mt-0.5 max-w-2xl text-[13px] text-fg-muted">{subtitle}</p>}
      </div>
      {actions && <div className="flex items-center gap-2">{actions}</div>}
    </div>
  );
}

/**
 * One bordered strip of headline numbers split by hairlines (Vercel / Linear
 * insights style) instead of a row of separate cards.
 */
export function MetricStrip({
  items,
  className,
}: {
  items: { label: string; value: number | undefined; hint?: ReactNode; icon?: ReactNode }[];
  className?: string;
}) {
  return (
    <div className={cx("grid grid-cols-2 overflow-hidden rounded-xl border border-line bg-surface lg:grid-cols-4", className)}>
      {items.map((m, i) => {
        return (
          <div key={m.label} className={cx("px-4 py-3.5", i > 0 && "border-l border-line", i === 2 && "max-lg:border-l-0 max-lg:border-t", i === 3 && "max-lg:border-t")}>
            <div className="flex items-center gap-1.5 text-xs text-fg-muted">
              {m.icon && <span className="text-fg-dim [&>svg]:size-3.5">{m.icon}</span>}
              {m.label}
            </div>
            <div className="mt-2 flex items-baseline gap-2">
              <span className="text-[24px] leading-none font-semibold tracking-tight tabular-nums">
                {m.value === undefined ? <Skeleton className="h-6 w-8" /> : <CountUp value={m.value} />}
              </span>
              {m.hint && <span className="truncate text-[11px] text-fg-dim">{m.hint}</span>}
            </div>
          </div>
        );
      })}
    </div>
  );
}

export function CountUp({ value, decimals = 0, suffix = "" }: { value: number; decimals?: number; suffix?: string }) {
  const ref = useRef<HTMLSpanElement>(null);
  const prev = useRef(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const controls = animate(prev.current, value, {
      duration: 0.6,
      ease: [0.22, 1, 0.36, 1],
      onUpdate: (v) => (el.textContent = v.toFixed(decimals) + suffix),
    });
    prev.current = value;
    return () => controls.stop();
  }, [value, decimals, suffix]);
  return <span ref={ref}>{(0).toFixed(decimals) + suffix}</span>;
}

export function Skeleton({ className }: { className?: string }) {
  return <span className={cx("inline-block animate-pulse rounded-md bg-surface-3", className)} />;
}

// --- buttons -----------------------------------------------------------------

type Variant = "primary" | "accent" | "subtle" | "ghost" | "danger";

const BUTTON: Record<Variant, string> = {
  primary: "bg-fg text-inverse font-medium hover:bg-fg/90 shadow-[0_1px_0_0_rgb(255_255_255/0.15)_inset]",
  accent: "bg-accent text-white font-medium hover:brightness-110 shadow-[0_1px_0_0_rgb(255_255_255/0.2)_inset]",
  subtle: "bg-surface-2 text-fg border border-line-strong hover:bg-surface-3",
  ghost: "text-fg-muted hover:text-fg hover:bg-fg/[0.06]",
  danger: "bg-bad/10 text-bad border border-bad/25 hover:bg-bad/15",
};

export function Button({
  variant = "subtle",
  busy,
  icon,
  children,
  className,
  size = "md",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; busy?: boolean; icon?: ReactNode; size?: "sm" | "md" }) {
  return (
    <button
      type="button"
      className={cx(
        "inline-flex shrink-0 items-center justify-center gap-1.5 rounded-lg whitespace-nowrap transition-[background,color,filter,box-shadow] duration-150 active:translate-y-px disabled:pointer-events-none disabled:opacity-45",
        size === "sm" ? "h-7 px-2.5 text-xs [&_svg]:size-3.5" : "h-8 px-3 text-[13px] [&_svg]:size-4",
        BUTTON[variant],
        className,
      )}
      disabled={busy || rest.disabled}
      {...rest}
    >
      {busy ? <Loader2 className="animate-spin" /> : icon}
      {children}
    </button>
  );
}

export function IconButton({
  label,
  children,
  className,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <Tooltip content={label}>
      <button
        type="button"
        aria-label={label}
        className={cx(
          "inline-flex size-7 shrink-0 items-center justify-center rounded-md text-fg-muted transition hover:bg-fg/[0.07] hover:text-fg disabled:pointer-events-none disabled:opacity-40 [&_svg]:size-4",
          className,
        )}
        {...rest}
      >
        {children}
      </button>
    </Tooltip>
  );
}

export function Kbd({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <kbd
      className={cx(
        "inline-flex h-5 min-w-5 items-center justify-center rounded border border-line-strong bg-surface-2 px-1 font-sans text-[11px] font-medium text-fg-muted",
        className,
      )}
    >
      {children}
    </kbd>
  );
}

// --- tooltip -------------------------------------------------------------------

/** Small delayed tooltip rendered in a portal, so cards never clip it. */
export function Tooltip({ content, children }: { content: ReactNode; children: ReactElement<Record<string, unknown>> }) {
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const tipRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  const show = (el: HTMLElement) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setAnchor(el.getBoundingClientRect()), 450);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    setAnchor(null);
    setPos(null);
  };
  useEffect(() => () => window.clearTimeout(timer.current), []);

  useLayoutEffect(() => {
    if (!anchor || !tipRef.current) return;
    const t = tipRef.current.getBoundingClientRect();
    const above = anchor.top - t.height - 8;
    const left = Math.min(window.innerWidth - t.width - 8, Math.max(8, anchor.left + anchor.width / 2 - t.width / 2));
    setPos({ left, top: above > 8 ? above : anchor.bottom + 8 });
  }, [anchor]);

  const child = children;
  const props = child.props as {
    onMouseEnter?: (e: React.MouseEvent<HTMLElement>) => void;
    onMouseLeave?: (e: React.MouseEvent<HTMLElement>) => void;
    onFocus?: (e: React.FocusEvent<HTMLElement>) => void;
    onBlur?: (e: React.FocusEvent<HTMLElement>) => void;
    onClick?: (e: React.MouseEvent<HTMLElement>) => void;
  };
  return (
    <>
      {cloneElement(child, {
        onMouseEnter: (e: React.MouseEvent<HTMLElement>) => {
          show(e.currentTarget);
          props.onMouseEnter?.(e);
        },
        onMouseLeave: (e: React.MouseEvent<HTMLElement>) => {
          hide();
          props.onMouseLeave?.(e);
        },
        onFocus: (e: React.FocusEvent<HTMLElement>) => {
          if (e.currentTarget.matches(":focus-visible")) show(e.currentTarget);
          props.onFocus?.(e);
        },
        onBlur: (e: React.FocusEvent<HTMLElement>) => {
          hide();
          props.onBlur?.(e);
        },
        onClick: (e: React.MouseEvent<HTMLElement>) => {
          hide();
          props.onClick?.(e);
        },
      })}
      {anchor &&
        createPortal(
          <div
            ref={tipRef}
            role="tooltip"
            className="pointer-events-none fixed z-[80] rounded-md border border-line-strong bg-overlay px-2 py-1 text-xs whitespace-nowrap text-fg shadow-lg"
            style={{ left: pos?.left ?? -9999, top: pos?.top ?? -9999 }}
          >
            {content}
          </div>,
          document.body,
        )}
    </>
  );
}

// --- status ------------------------------------------------------------------

const DOT: Record<Health, string> = {
  ok: "bg-ok",
  warn: "bg-warn",
  bad: "bg-bad",
  idle: "bg-fg-dim",
};

/** A small flat dot. No glow or ping: status is quiet unless something is wrong. */
export function StatusDot({ health }: { health: Health }) {
  return <span className={cx("inline-block size-1.5 shrink-0 rounded-full", DOT[health])} />;
}

// --- data ----------------------------------------------------------------------

/** Usage bar: high is bad by default; `battery` flips that (low is bad). */
export function Meter({ value, tone, label }: { value: number; tone?: "battery"; label?: string }) {
  const v = Math.max(0, Math.min(100, value));
  // One neutral fill; colour only appears when a threshold is crossed.
  const color =
    tone === "battery" ? (v < 20 ? "bg-bad" : v < 35 ? "bg-warn" : "bg-fg-muted") : v > 90 ? "bg-bad" : v > 75 ? "bg-warn" : "bg-fg-muted";
  return (
    <div
      className="h-1 w-full overflow-hidden rounded-full bg-fg/[0.08]"
      role="meter"
      aria-valuenow={Math.round(v)}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label={label}
    >
      <motion.div
        className={cx("h-full rounded-full", color)}
        initial={{ width: 0 }}
        animate={{ width: `${v}%` }}
        transition={{ type: "spring", stiffness: 120, damping: 22 }}
      />
    </div>
  );
}

/**
 * Ember progress track for transfers (model downloads, .env saves): a
 * gradient fill and, while work is running, a beam sweeping across it.
 */
export function ProgressBar({ value, active, label }: { value: number; active?: boolean; label: string }) {
  const v = Math.max(0, Math.min(100, value));
  return (
    <div className="relative h-1 overflow-hidden rounded-full bg-fg/[0.08]" role="progressbar" aria-valuenow={Math.round(v)} aria-valuemin={0} aria-valuemax={100} aria-label={label}>
      <motion.div
        className="h-full rounded-full"
        style={{ background: "linear-gradient(90deg, var(--accent), var(--accent-soft))" }}
        initial={false}
        animate={{ width: `${Math.max(3, v)}%` }}
        transition={{ type: "spring", stiffness: 120, damping: 24 }}
      />
      {active && (
        <span
          className="absolute inset-y-0 left-0 w-1/4"
          style={{ background: "linear-gradient(90deg, transparent, rgb(255 255 255 / 0.55), transparent)", animation: "beam 1.3s ease-in-out infinite" }}
        />
      )}
    </div>
  );
}

export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={cx("size-4 animate-spin text-fg-dim", className)} />;
}

export function Loading({ rows = 3 }: { rows?: number }) {
  return (
    <div className="space-y-2.5" aria-busy="true">
      <ChaosDots />
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className={cx("block h-9", i % 2 ? "w-4/5" : "w-full")} />
      ))}
    </div>
  );
}

export function EmptyState({ icon, title, children, action }: { icon: ReactNode; title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 px-6 py-10 text-center">
      <div className="mb-1 flex size-10 items-center justify-center rounded-xl border border-line-strong bg-surface-2 text-fg-muted [&>svg]:size-5">
        {icon}
      </div>
      <div className="text-sm font-medium">{title}</div>
      {children && <div className="max-w-md text-[13px] leading-relaxed text-fg-muted">{children}</div>}
      {action && <div className="mt-2">{action}</div>}
    </div>
  );
}

export function ErrorNote({ error, className }: { error?: { message: string } | null; className?: string }) {
  if (!error) return null;
  return (
    <div role="alert" className={cx("selectable rounded-lg border border-bad/25 bg-bad/[0.07] px-3 py-2 text-[13px] text-bad", className)}>
      {error.message}
    </div>
  );
}

export function Callout({ tone = "warn", title, children, className }: { tone?: "warn" | "bad" | "idle"; title?: ReactNode; children: ReactNode; className?: string }) {
  const cls = tone === "warn" ? "border-warn/25 bg-warn/[0.06]" : tone === "bad" ? "border-bad/25 bg-bad/[0.06]" : "border-line bg-surface-2";
  return (
    <div className={cx("rounded-lg border px-3.5 py-3 text-[13px]", cls, className)}>
      {title && <div className={cx("mb-0.5 font-medium", tone === "warn" ? "text-warn" : tone === "bad" ? "text-bad" : "text-fg")}>{title}</div>}
      <div className="text-fg-muted">{children}</div>
    </div>
  );
}

/** A checkbox with a label and a one-line hint. */
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

export function InlineCode({ children }: { children: ReactNode }) {
  return <code className="rounded border border-line bg-surface-2 px-1 py-px font-mono text-[12px] text-fg">{children}</code>;
}

// --- motion ------------------------------------------------------------------
// Loading and progress pieces adapted from morphin.dev's text shimmer wave,
// pixel wave, chaotic dots loader, animated status badge and orbiting-glow
// button. The keyframes live in index.css; React only lays out the elements.

/** Loading label whose letters lift and brighten in a wave. */
export function ShimmerText({ children, className }: { children: string; className?: string }) {
  return (
    <span className={cx("shimmer-wave whitespace-nowrap", className)} role="status" aria-label={children}>
      {Array.from(children, (c, i) => (
        <span key={i} aria-hidden="true" style={{ "--i": i } as React.CSSProperties}>
          {c === " " ? "\u00a0" : c}
        </span>
      ))}
    </span>
  );
}

/** A small square grid lit along its diagonal, for waits with no progress to show. */
export function PixelWave({ size = 3, pixel = 5, gap = 2, className }: { size?: number; pixel?: number; gap?: number; className?: string }) {
  return (
    <span
      className={cx("pixel-wave inline-grid shrink-0", className)}
      style={{ gridTemplateColumns: `repeat(${size}, ${pixel}px)`, gap }}
      aria-hidden="true"
    >
      {Array.from({ length: size * size }, (_, i) => (
        <span key={i} style={{ width: pixel, height: pixel, "--d": (i % size) + Math.floor(i / size) } as React.CSSProperties} />
      ))}
    </span>
  );
}

/** Two rows of dots twinkling out of step: a loading strip above table skeletons. */
export function ChaosDots({ className }: { className?: string }) {
  const dots = useMemo(
    () =>
      Array.from({ length: 2 * 96 }, (_, i) => ({
        d: `${(Math.random() * 2.4).toFixed(2)}s`,
        t: `${(1.1 + Math.random() * 1.6).toFixed(2)}s`,
        hot: (i * 7919) % 13 === 0,
      })),
    [],
  );
  return (
    <div className={cx("chaos-dots grid h-[10px] grid-flow-col grid-rows-2 justify-between gap-1 overflow-hidden", className)} aria-hidden="true">
      {dots.map((x, i) => (
        <span key={i} data-hot={x.hot || undefined} style={{ "--d": x.d, "--t": x.t } as React.CSSProperties} />
      ))}
    </div>
  );
}

/**
 * Dot + label that animates when the status changes: the old word rolls up
 * and out, the new one rolls in, and the row flashes once in the new tone.
 * Nothing moves while the status stays the same.
 */
export function StatusText({ health, label, className, testId }: { health: Health; label: string; className?: string; testId?: string }) {
  // Count real changes only: polling re-renders with the same label never flash.
  const prev = useRef(label);
  const [changes, setChanges] = useState(0);
  useEffect(() => {
    if (prev.current === label) return;
    prev.current = label;
    setChanges((n) => n + 1);
  }, [label]);
  return (
    <span data-testid={testId} className={cx("relative inline-flex min-w-0 items-center gap-1.5", className)}>
      <span className={cx("inline-block size-1.5 shrink-0 rounded-full transition-colors duration-500", DOT[health])} />
      <span className="relative inline-grid min-w-0 overflow-hidden">
        <AnimatePresence mode="popLayout" initial={false}>
          <motion.span
            key={label}
            className="truncate"
            initial={{ y: "90%", opacity: 0, filter: "blur(2px)" }}
            animate={{ y: 0, opacity: 1, filter: "blur(0px)" }}
            exit={{ y: "-90%", opacity: 0, filter: "blur(2px)" }}
            transition={{ type: "spring", stiffness: 420, damping: 32 }}
          >
            {label}
          </motion.span>
        </AnimatePresence>
      </span>
      {changes > 0 && (
        <motion.span
          key={changes}
          aria-hidden="true"
          className={cx("pointer-events-none absolute -inset-x-1.5 -inset-y-0.5 rounded-md", DOT[health])}
          initial={{ opacity: 0.22 }}
          animate={{ opacity: 0 }}
          transition={{ duration: 1.1, ease: "easeOut" }}
        />
      )}
    </span>
  );
}

type OrbitState = "idle" | "busy" | "done";

/**
 * Primary action that shows its own progress: on click it darkens, an ember
 * arc orbits its border and a few sparks drift while the work runs, then it
 * settles on a check before returning to rest.
 */
export function OrbitButton({
  children,
  busyLabel,
  doneLabel = "Done",
  icon,
  onRun,
  size = "md",
  className,
  testId,
  type = "button",
  disabled,
  busy,
}: {
  children: ReactNode;
  busyLabel: string;
  doneLabel?: string;
  icon?: ReactNode;
  /** Resolve to finish; throw to return to rest without the check. */
  onRun?: () => Promise<unknown>;
  size?: "sm" | "md";
  className?: string;
  testId?: string;
  type?: "button" | "submit";
  disabled?: boolean;
  /** Controlled mode for submit buttons: the form runs the work, this only shows it. */
  busy?: boolean;
}) {
  const [own, setState] = useState<OrbitState>("idle");
  const state: OrbitState = busy === undefined ? own : busy ? "busy" : "idle";
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const sparks = useMemo(() => Array.from({ length: 7 }, () => ({ l: 8 + Math.random() * 84, t: 20 + Math.random() * 60, d: Math.random() * 2.4 })), []);

  async function run() {
    if (!onRun || state !== "idle") return;
    setState("busy");
    try {
      // Hold the busy look briefly even when the server answers at once, so it reads as a step, not a flicker.
      await Promise.all([onRun(), new Promise((r) => setTimeout(r, 800))]);
      setState("done");
      timer.current = window.setTimeout(() => setState("idle"), 1600);
    } catch {
      setState("idle");
    }
  }

  const resting = state === "idle";
  return (
    <motion.button
      layout
      type={type}
      data-testid={testId}
      data-state={state}
      disabled={disabled || (!resting && type === "submit")}
      onClick={type === "submit" ? undefined : () => void run()}
      aria-busy={state === "busy"}
      transition={{ layout: { type: "spring", stiffness: 500, damping: 38 } }}
      className={cx(
        "relative inline-flex shrink-0 items-center justify-center gap-1.5 overflow-visible rounded-lg font-medium whitespace-nowrap transition-[background-color,color,box-shadow] duration-300 disabled:opacity-45",
        size === "sm" ? "h-7 px-2.5 text-xs [&_svg]:size-3.5" : "h-8 px-3 text-[13px] [&_svg]:size-4",
        resting
          ? "bg-fg text-inverse shadow-[0_1px_0_0_rgb(255_255_255/0.15)_inset] hover:bg-fg/90 active:translate-y-px"
          : "bg-surface-3 text-fg shadow-[0_0_0_1px_var(--line-strong)]",
        className,
      )}
    >
      {state === "busy" && (
        <>
          <span className="orbit-ring" data-glow="" />
          <span className="orbit-ring" />
          <span className="pointer-events-none absolute inset-0 overflow-hidden rounded-[inherit]">
            {sparks.map((p, i) => (
              <span key={i} className="orbit-spark" style={{ left: `${p.l}%`, top: `${p.t}%`, "--d": `${p.d}s` } as React.CSSProperties} />
            ))}
          </span>
        </>
      )}
      <AnimatePresence mode="popLayout" initial={false}>
        <motion.span
          key={state}
          layout="position"
          className="relative inline-flex items-center gap-1.5"
          initial={{ opacity: 0, y: 6, filter: "blur(3px)" }}
          animate={{ opacity: 1, y: 0, filter: "blur(0px)" }}
          exit={{ opacity: 0, y: -6, filter: "blur(3px)" }}
          transition={{ duration: 0.22, ease: [0.22, 1, 0.36, 1] }}
        >
          {state === "idle" && (
            <>
              {icon}
              {children}
            </>
          )}
          {state === "busy" && (
            <>
              <Loader2 className="animate-spin text-fg-muted" />
              <ShimmerText>{busyLabel}</ShimmerText>
            </>
          )}
          {state === "done" && (
            <>
              <Check className="text-ok" />
              {doneLabel}
            </>
          )}
        </motion.span>
      </AnimatePresence>
    </motion.button>
  );
}

// --- forms ---------------------------------------------------------------------

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="flex min-w-0 flex-col gap-1.5">
      <span className="text-xs font-medium text-fg-muted">{label}</span>
      {children}
      {hint && <span className="text-[11px] leading-snug text-fg-dim">{hint}</span>}
    </label>
  );
}

const inputCls =
  "h-8 w-full rounded-lg border border-line-strong bg-bg px-2.5 text-[13px] text-fg placeholder:text-fg-dim outline-none transition focus:border-accent/60 focus:ring-3 focus:ring-accent/15 disabled:opacity-50";

export function TextInput(props: InputHTMLAttributes<HTMLInputElement>) {
  return <input {...props} className={cx(inputCls, props.className)} />;
}

export function Select({ className, ...props }: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <div className={cx("relative", className)}>
      <select {...props} className={cx(inputCls, "appearance-none pr-8")} />
      <ChevronDown className="pointer-events-none absolute top-1/2 right-2.5 size-3.5 -translate-y-1/2 text-fg-dim" />
    </div>
  );
}

/** Segmented control. */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  label,
}: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: ReactNode; testId?: string }[];
  label: string;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="relative inline-flex h-8 self-start rounded-lg border border-line bg-bg p-0.5">
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={active}
            data-testid={o.testId}
            onClick={() => onChange(o.value)}
            className={cx(
              "relative z-10 inline-flex items-center gap-1.5 rounded-md px-2.5 text-xs font-medium whitespace-nowrap transition [&_svg]:size-3.5",
              active ? "text-fg" : "text-fg-muted hover:text-fg",
            )}
          >
            {active && (
              <motion.span
                layoutId={`seg-${label}`}
                className="absolute inset-0 -z-10 rounded-md border border-line-strong bg-surface-3"
                transition={{ type: "spring", stiffness: 500, damping: 36 }}
              />
            )}
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/**
 * Tabs for switching views inside a page or sheet. Every tab always shows its
 * label (an icon alone makes newcomers guess), the active one is underlined
 * and the strip scrolls sideways when the window is narrow.
 */
export function Tabs<T extends string>({
  value,
  onChange,
  items,
  label,
  className,
}: {
  value: T;
  onChange: (v: T) => void;
  items: { value: T; label: string; icon?: ReactNode; testId?: string; count?: number }[];
  label: string;
  className?: string;
}) {
  return (
    <div role="tablist" aria-label={label} className={cx("-mb-px flex items-end gap-1 overflow-x-auto [scrollbar-width:none]", className)}>
      {items.map((i) => {
        const active = i.value === value;
        return (
          <button
            key={i.value}
            type="button"
            role="tab"
            aria-selected={active}
            data-testid={i.testId}
            onClick={() => onChange(i.value)}
            className={cx(
              "relative flex h-10 shrink-0 items-center gap-1.5 px-2.5 text-[13px] whitespace-nowrap transition-colors [&_svg]:size-4",
              active ? "font-medium text-fg" : "text-fg-muted hover:text-fg",
            )}
          >
            {i.icon && <span className={cx("flex", active ? "text-fg-muted" : "text-fg-dim")}>{i.icon}</span>}
            {i.label}
            {!!i.count && <span className="rounded-full bg-fg/[0.07] px-1.5 text-[10px] text-fg-muted tabular-nums">{i.count}</span>}
            {active && (
              <motion.span
                layoutId={`tabs-${label}`}
                className="absolute inset-x-1.5 bottom-0 h-0.5 rounded-full bg-fg"
                transition={{ type: "spring", stiffness: 520, damping: 40 }}
              />
            )}
          </button>
        );
      })}
    </div>
  );
}

/** Animates its height to fit its content (the dock's panel resize). */
export function AutoHeight({ children, className }: { children: ReactNode; className?: string }) {
  const inner = useRef<HTMLDivElement>(null);
  const [h, setH] = useState<number | "auto">("auto");
  useLayoutEffect(() => {
    const el = inner.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setH(el.offsetHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return (
    <motion.div className={cx("-m-1 overflow-hidden p-1", className)} animate={{ height: h === "auto" ? "auto" : h + 8 }} initial={false} transition={{ type: "spring", stiffness: 320, damping: 36 }}>
      <div ref={inner}>{children}</div>
    </motion.div>
  );
}

// --- copy & code -------------------------------------------------------------

export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

export function CopyButton({ text, label = "Copy", className }: { text: string; label?: string; className?: string }) {
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(() => setDone(false), 1500);
    return () => clearTimeout(t);
  }, [done]);
  return (
    <Button
      size="sm"
      variant="subtle"
      className={className}
      icon={done ? <Check className="text-ok" /> : <Copy />}
      onClick={() => copyText(text).then(() => setDone(true))}
    >
      {done ? "Copied" : label}
    </Button>
  );
}

export function CodeBlock({ code, testId, prompt }: { code: string; testId?: string; prompt?: boolean }) {
  if (prompt) {
    // One-line shell command: scrolls on its own, copy button beside it.
    return (
      <div className="flex items-center gap-2 rounded-lg border border-line bg-bg py-1.5 pr-1.5 pl-3.5">
        <pre data-testid={testId} className="min-w-0 flex-1 overflow-x-auto py-1 font-mono text-[12px] text-fg [scrollbar-width:none]">
          <span className="text-fg-dim select-none">$ </span>
          {code}
        </pre>
        <CopyButton text={code} />
      </div>
    );
  }
  return (
    <div className="group relative">
      <pre
        data-testid={testId}
        className="max-h-96 overflow-auto rounded-lg border border-line bg-bg p-3.5 pr-24 font-mono text-[12px] leading-relaxed text-fg"
      >
        {code}
      </pre>
      <div className="absolute top-2 right-2">
        <CopyButton text={code} />
      </div>
    </div>
  );
}

export function LogView({ children, testId, className }: { children: ReactNode; testId?: string; className?: string }) {
  return (
    <pre
      data-testid={testId}
      className={cx(
        "max-h-[62vh] overflow-auto rounded-lg border border-line bg-bg p-3.5 font-mono text-[11.5px] leading-[1.7] whitespace-pre-wrap text-fg-muted",
        className,
      )}
    >
      {children}
    </pre>
  );
}

// --- overlays ------------------------------------------------------------------

export function Modal({
  open,
  onClose,
  title,
  description,
  children,
  wide,
  sheet,
  actions,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  wide?: boolean;
  /** Slide in from the right as a full-height panel. */
  sheet?: boolean;
  /** Buttons beside the close button, for the sheet's main actions. */
  actions?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const wasOpen = useRef(false);
  const returnFocus = useRef<HTMLElement | null>(null);
  if (open && !wasOpen.current) returnFocus.current = document.activeElement as HTMLElement | null;
  wasOpen.current = open;
  useEffect(() => {
    if (!open) return;
    const previous = returnFocus.current;
    const dialog = ref.current;
    if (dialog && !dialog.contains(document.activeElement)) dialog.focus();
    return () => { if (previous?.isConnected) previous.focus(); };
  }, [open]);
  useEffect(() => {
    if (!open) return;
    // Only the innermost open modal reacts to Escape (log viewers open on
    // top of detail dialogs). It consumes the key: a closed inner modal can
    // be gone from the DOM before the outer one's listener runs.
    const onKey = (e: KeyboardEvent) => {
      if (!["Escape", "Tab"].includes(e.key) || e.defaultPrevented || !ref.current) return;
      const dialogs = Array.from(document.querySelectorAll<HTMLElement>("[data-modal]"));
      const innermost = dialogs.filter((d) => !dialogs.some((o) => o !== d && d.contains(o)));
      if (innermost[innermost.length - 1] !== ref.current) return;
      if (e.key === "Tab") {
        const focusable = Array.from(ref.current.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex="0"]')).filter((el) => !el.closest('[hidden], [inert]'));
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        if (!first || !last) { e.preventDefault(); ref.current.focus(); }
        else if (e.shiftKey && (document.activeElement === first || !ref.current.contains(document.activeElement))) { e.preventDefault(); last.focus(); }
        else if (!e.shiftKey && (document.activeElement === last || document.activeElement === ref.current || !ref.current.contains(document.activeElement))) { e.preventDefault(); first.focus(); }
        return;
      }
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);
  return (
    <AnimatePresence>
      {open && (
        <motion.div
          className={cx(
            "fixed inset-0 z-50 flex bg-black/50 backdrop-blur-[2px]",
            sheet ? "justify-end" : "items-center justify-center p-6",
          )}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.15 }}
          onMouseDown={(e) => e.target === e.currentTarget && onClose()}
        >
          <motion.div
            ref={ref}
            data-modal
            tabIndex={-1}
            role="dialog"
            aria-modal="true"
            aria-labelledby={titleId}
            className={cx(
              "flex w-full flex-col border border-line-strong bg-overlay shadow-lg",
              sheet ? "h-full max-w-[min(940px,92vw)] border-y-0 border-r-0" : "max-h-[85vh] rounded-xl",
              !sheet && (wide ? "max-w-5xl" : "max-w-lg"),
            )}
            initial={sheet ? { x: 40, opacity: 0 } : { y: 10, scale: 0.98, opacity: 0 }}
            animate={sheet ? { x: 0, opacity: 1 } : { y: 0, scale: 1, opacity: 1 }}
            exit={sheet ? { x: 40, opacity: 0 } : { y: 6, scale: 0.98, opacity: 0 }}
            transition={{ type: "spring", stiffness: 420, damping: 38 }}
          >
            <div className={cx("flex items-start justify-between gap-4 border-b border-line px-5", sheet ? "pt-11 pb-4" : "py-3.5")}>
              <div className="min-w-0">
                <div id={titleId} className="truncate text-[15px] font-semibold tracking-tight">
                  {title}
                </div>
                {description && <div className="mt-0.5 text-xs text-fg-muted">{description}</div>}
              </div>
              <div className="flex shrink-0 items-center gap-1.5">
                {actions}
                <IconButton label="Close" onClick={onClose}>
                  <X />
                </IconButton>
              </div>
            </div>
            <div className="min-h-0 flex-1 overflow-auto p-5">{children}</div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
