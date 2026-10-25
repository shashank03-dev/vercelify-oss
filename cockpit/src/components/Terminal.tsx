// A shell in a container (`docker exec -it` on the server). xterm.js is
// loaded only when a terminal opens; its colours come from the theme tokens,
// so it follows dark and light mode. Keystrokes are batched and sent in
// order; output arrives as a stream.

import { useEffect, useRef, useState } from "react";
import { fromBase64, type PlatformClient } from "../lib/platform";
import type { TerminalSession } from "../types";
import { Callout, Loading } from "./ui";

function token(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

export function TerminalView({
  client,
  open,
  testId = "terminal",
}: {
  client: PlatformClient;
  /** Opens the session at the given size. */
  open: (cols: number, rows: number) => Promise<TerminalSession>;
  testId?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<{ phase: "loading" | "open" | "ended"; error?: string; code?: number }>({ phase: "loading" });
  const openRef = useRef(open);
  openRef.current = open;

  useEffect(() => {
    let disposed = false;
    const cleanups: (() => void)[] = [];
    void (async () => {
      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import("@xterm/xterm"),
        import("@xterm/addon-fit"),
        import("@xterm/xterm/css/xterm.css"),
      ]);
      if (disposed || !box.current) return;
      const term = new Terminal({
        fontFamily: '"Geist Mono Variable", ui-monospace, monospace',
        fontSize: 12.5,
        cursorBlink: true,
        scrollback: 5000,
        theme: {
          background: token("--color-bg"),
          foreground: token("--color-fg"),
          cursor: token("--color-accent"),
          selectionBackground: token("--color-surface-3"),
        },
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(box.current);
      fit.fit();
      cleanups.push(() => term.dispose());

      let session: TerminalSession;
      try {
        session = await openRef.current(term.cols, term.rows);
      } catch (e) {
        if (!disposed) setState({ phase: "ended", error: (e as Error).message });
        return;
      }
      if (disposed) {
        void client.closeTerminal(session.id).catch(() => {});
        return;
      }
      setState({ phase: "open" });
      cleanups.push(() => void client.closeTerminal(session.id).catch(() => {}));

      const out = client.terminalOutput(session.id, (e) => {
        if ("data" in e) term.write(fromBase64(e.data));
        else {
          term.write(`\r\n\x1b[2m[exited with code ${e.exit}]\x1b[0m\r\n`);
          setState({ phase: "ended", code: e.exit });
        }
      });
      out.done.catch((e: Error) => !disposed && setState({ phase: "ended", error: e.message }));
      cleanups.push(() => out.cancel());

      // Keystrokes: batched for a few milliseconds, sent one request at a time.
      let pending = "";
      let sending: Promise<unknown> = Promise.resolve();
      let timer = 0;
      const flush = () => {
        timer = 0;
        const data = pending;
        pending = "";
        if (!data) return;
        sending = sending.then(() => client.terminalInput(session.id, data)).catch(() => {});
      };
      const onData = term.onData((d) => {
        pending += d;
        if (!timer) timer = window.setTimeout(flush, 12);
      });
      cleanups.push(() => onData.dispose());

      let resizeTimer = 0;
      const onResize = term.onResize(({ cols, rows }) => {
        window.clearTimeout(resizeTimer);
        resizeTimer = window.setTimeout(() => void client.terminalResize(session.id, cols, rows).catch(() => {}), 150);
      });
      cleanups.push(() => onResize.dispose());
      const ro = new ResizeObserver(() => fit.fit());
      ro.observe(box.current);
      cleanups.push(() => ro.disconnect());
      term.focus();
    })();
    return () => {
      disposed = true;
      cleanups.reverse().forEach((f) => f());
    };
  }, [client]);

  return (
    <div className="space-y-2">
      {state.error && <Callout tone="bad">{state.error}</Callout>}
      {state.phase === "loading" && <Loading rows={2} />}
      <div ref={box} data-testid={testId} data-state={state.phase} className="h-[52vh] overflow-hidden rounded-lg border border-line bg-bg p-2" />
      <p className="text-[11px] text-fg-dim">
        {state.phase === "ended" ? "The session ended. Close and reopen the tab for a new one." : "A shell inside the running container. Changes to its files are lost on the next deploy."}
      </p>
    </div>
  );
}
