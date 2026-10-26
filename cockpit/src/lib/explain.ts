// "Why did my deploy fail?" — an open-weight model reads a failed build log.
// Same prompt and trimming as `vfy why` in cli/vfy (explain.test.ts keeps the
// two in sync). Small log, capped answer: an old laptop's CPU reads every line.

import type { LlmChatMessage } from "../types";

/** Gemma 4 E2B: ~2.3B effective parameters, Apache 2.0, runs on a CPU. */
export const EXPLAIN_MODEL = "gemma4:e2b";
export const EXPLAIN_MAX_TOKENS = 400;
export const EXPLAIN_LOG_CHARS = 5000;

export const EXPLAIN_SYSTEM = `You are Vercelify's deploy doctor. A deployment failed on a self-hosted Vercelify server:
Vercelify builds the app with its build pack or a Dockerfile inside a Linux container on an old laptop.
Linux file names are case-sensitive, the build cannot see the developer's machine,
and environment variables come only from the app's settings.
Read the build log and answer in exactly this format, under 120 words:
**Cause:** one sentence naming the failing step and the root cause.
**Fix:** the exact change (file and line, setting, or command).
**Check:** one command to confirm the fix locally before pushing again.
Use only facts from the log. If the log does not show the cause, say what to look for next.`;

const ERROR_LINE =
  /(error|err!|failed|failure|fatal|cannot|can.t|could not|not found|no such file|denied|killed|out of memory|exit code|exited with|segmentation|panic|traceback|exception|missing|undefined|unresolved|enoent|eacces|enomem)/;
const CONTEXT_BEFORE = 2;
const CONTEXT_AFTER = 3;
const TAIL = 30;

/** The error lines with context, then the last 30 lines; "…" marks gaps. */
export function trimLog(lines: string[], budget = EXPLAIN_LOG_CHARS): string {
  const keep = new Set<number>();
  lines.forEach((l, i) => {
    if (ERROR_LINE.test(l.toLowerCase())) for (let j = i - CONTEXT_BEFORE; j <= i + CONTEXT_AFTER; j++) keep.add(j);
  });
  for (let j = lines.length - TAIL; j < lines.length; j++) keep.add(j);
  const out: string[] = [];
  let prev = -1;
  lines.forEach((l, i) => {
    if (!keep.has(i)) return;
    if (i > prev + 1) out.push("…");
    out.push(l);
    prev = i;
  });
  const text = out.join("\n");
  // Over budget: the tail matters most (the failing step is near the end).
  return text.length <= budget ? text : "…" + text.slice(-(budget - 1));
}

export interface ExplainContext {
  app?: string;
  commit?: string | null;
  commitMessage?: string | null;
  vercelJson?: string | null;
}

/** Build-log lines: plain text, or lines with their stream. */
export type ExplainLine = string | { output: string; type?: string | null };

/** The chat messages for one failed build, as `vfy why` sends them. */
export function explainMessages(log: ExplainLine[] | string, ctx: ExplainContext = {}): LlmChatMessage[] {
  const lines =
    typeof log === "string"
      ? log.replace(/\n+$/, "").split("\n")
      : log.map((l) => (typeof l === "string" ? l : (l.type === "stderr" ? "[stderr] " : "") + l.output));
  const head = ctx.app ? `App: ${ctx.app}\nCommit: ${(ctx.commit ?? "").slice(0, 7)} "${(ctx.commitMessage ?? "").split("\n")[0]}"\n\n` : "";
  let user = `${head}Build log (error lines with context, then the last lines):\n\`\`\`\n${trimLog(lines)}\n\`\`\``;
  if (ctx.vercelJson) user += `\n\nvercel.json:\n\`\`\`json\n${ctx.vercelJson.slice(0, 1500)}\n\`\`\``;
  return [
    { role: "system", content: EXPLAIN_SYSTEM },
    { role: "user", content: user },
  ];
}

/** Gemma 4 E2B if installed, then another Gemma 4, then any chat model. */
export function pickExplainModel(installed: string[]): string | undefined {
  if (installed.includes(EXPLAIN_MODEL)) return EXPLAIN_MODEL;
  return installed.find((n) => n.startsWith("gemma4")) ?? installed.find((n) => !/embed/i.test(n));
}

/** Is this chat the deploy-doctor prompt? (demo mode answers it differently) */
export function isExplainChat(messages: LlmChatMessage[]): boolean {
  return messages[0]?.role === "system" && messages[0].content === EXPLAIN_SYSTEM;
}
