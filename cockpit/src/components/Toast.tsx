import { AnimatePresence, motion } from "framer-motion";
import { AlertTriangle, CheckCircle2, Info } from "lucide-react";
import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { cx, PixelWave, ShimmerText } from "./ui";

type Tone = "ok" | "bad" | "info" | "loading";
interface Toast {
  id: number;
  tone: Tone;
  text: string;
  /** Bumped when a progress toast resolves, which replays the ripple. */
  rev: number;
}

/** Something the user was told this session; the bell keeps the last few. */
export interface ActivityEvent {
  id: number;
  tone: Exclude<Tone, "loading">;
  text: string;
  at: number;
}

interface Progress {
  ok: (text: string) => void;
  bad: (text: string) => void;
}

interface Api {
  push: (tone: Exclude<Tone, "loading">, text: string) => void;
  progress: (text: string) => Progress;
  events: ActivityEvent[];
  unread: number;
  markRead: () => void;
}

const Ctx = createContext<Api | null>(null);
const LIFETIME = { ok: 3500, info: 3500, bad: 7000 } as const;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [events, setEvents] = useState<ActivityEvent[]>([]);
  const [seen, setSeen] = useState(0);
  const nextId = useRef(1);

  const record = useCallback((tone: ActivityEvent["tone"], text: string) => {
    setEvents((e) => [{ id: nextId.current++, tone, text, at: Date.now() }, ...e].slice(0, 30));
  }, []);

  const dismissLater = useCallback((id: number, tone: keyof typeof LIFETIME) => {
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), LIFETIME[tone]);
  }, []);

  const push = useCallback(
    (tone: ActivityEvent["tone"], text: string) => {
      const id = nextId.current++;
      setToasts((t) => [...t.slice(-3), { id, tone, text, rev: 0 }]);
      record(tone, text);
      dismissLater(id, tone);
    },
    [record, dismissLater],
  );

  const progress = useCallback(
    (text: string): Progress => {
      const id = nextId.current++;
      setToasts((t) => [...t.slice(-3), { id, tone: "loading", text, rev: 0 }]);
      const settle = (tone: "ok" | "bad") => (done: string) => {
        setToasts((t) => t.map((x) => (x.id === id ? { ...x, tone, text: done, rev: x.rev + 1 } : x)));
        record(tone, done);
        dismissLater(id, tone);
      };
      return { ok: settle("ok"), bad: settle("bad") };
    },
    [record, dismissLater],
  );

  const api = useMemo<Api>(
    () => ({ push, progress, events, unread: Math.max(0, events.length - seen), markRead: () => setSeen(events.length) }),
    [push, progress, events, seen],
  );

  return (
    <Ctx.Provider value={api}>
      {children}
      <div className="pointer-events-none fixed right-4 bottom-4 z-[90] flex w-[360px] flex-col gap-2" aria-live="polite">
        <AnimatePresence initial={false}>
          {toasts.map((t) => (
            <ToastCard key={t.id} toast={t} />
          ))}
        </AnimatePresence>
      </div>
    </Ctx.Provider>
  );
}

const TONE_TEXT = { ok: "text-ok", bad: "text-bad", info: "text-fg-muted", loading: "text-fg-muted" } as const;

/**
 * After morphin.dev's ripple toast: when a toast lands (or a progress toast
 * resolves) two rings spread from its icon, a soft wash of the tone fades
 * across the card and a hairline of light runs along the top edge.
 */
function ToastCard({ toast: t }: { toast: Toast }) {
  const ripple = t.tone === "ok" || t.tone === "bad";
  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 14, scale: 0.96 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, x: 18, transition: { duration: 0.16 } }}
      transition={{ type: "spring", stiffness: 480, damping: 36 }}
      className={cx("pointer-events-auto relative overflow-hidden rounded-xl border border-line-strong bg-overlay text-[13px] shadow-lg", TONE_TEXT[t.tone])}
      role={t.tone === "bad" ? "alert" : "status"}
    >
      {ripple && (
        <span key={`fx-${t.rev}`} aria-hidden="true">
          <motion.span
            className="pointer-events-none absolute inset-0"
            style={{ background: "radial-gradient(circle at 24px 50%, color-mix(in srgb, currentColor 16%, transparent), transparent 65%)" }}
            initial={{ opacity: 1 }}
            animate={{ opacity: 0 }}
            transition={{ duration: 1.4, ease: "easeOut" }}
          />
          <motion.span
            className="pointer-events-none absolute inset-x-8 top-0 h-px"
            style={{ background: "linear-gradient(90deg, transparent, currentColor, transparent)" }}
            initial={{ opacity: 0, scaleX: 0.2 }}
            animate={{ opacity: [0, 0.9, 0], scaleX: 1 }}
            transition={{ duration: 1.1, ease: "easeOut" }}
          />
        </span>
      )}
      <motion.div layout="position" className="relative flex items-start gap-2.5 px-3.5 py-3">
        <span className="relative mt-px flex size-4 shrink-0 items-center justify-center">
          {ripple &&
            [0, 0.18].map((delay) => (
              <motion.span
                key={`${t.rev}-${delay}`}
                aria-hidden="true"
                className="absolute inset-0 rounded-full border border-current"
                initial={{ scale: 0.8, opacity: 0.55 }}
                animate={{ scale: 2.8, opacity: 0 }}
                transition={{ duration: 0.9, delay, ease: [0.22, 1, 0.36, 1] }}
              />
            ))}
          <AnimatePresence mode="popLayout" initial={false}>
            <motion.span
              key={t.tone}
              className="flex"
              initial={{ scale: 0.4, opacity: 0, rotate: -30 }}
              animate={{ scale: 1, opacity: 1, rotate: 0 }}
              exit={{ scale: 0.4, opacity: 0 }}
              transition={{ type: "spring", stiffness: 520, damping: 26 }}
            >
              {t.tone === "ok" ? (
                <CheckCircle2 className="size-4" />
              ) : t.tone === "bad" ? (
                <AlertTriangle className="size-4" />
              ) : t.tone === "loading" ? (
                <PixelWave size={3} pixel={3} gap={1.5} />
              ) : (
                <Info className="size-4" />
              )}
            </motion.span>
          </AnimatePresence>
        </span>
        <span className="selectable min-w-0 leading-snug text-fg">{t.tone === "loading" ? <ShimmerText>{t.text}</ShimmerText> : t.text}</span>
      </motion.div>
    </motion.div>
  );
}

export function useToast() {
  const api = useContext(Ctx);
  if (!api) throw new Error("useToast outside ToastProvider");
  const { push, progress } = api;
  return useMemo(
    () => ({
      ok: (t: string) => push("ok", t),
      bad: (t: string) => push("bad", t),
      info: (t: string) => push("info", t),
      /** A toast that shows work in progress and turns into the result in place. */
      progress,
    }),
    [push, progress],
  );
}

/** Messages shown this session, newest first, with an unread count for the bell. */
export function useActivity() {
  const api = useContext(Ctx);
  if (!api) throw new Error("useActivity outside ToastProvider");
  return { events: api.events, unread: api.unread, markRead: api.markRead };
}
