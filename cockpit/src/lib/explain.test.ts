import failedBuild from "../../../cli/examples/failed-build.log?raw";
import vfy from "../../../cli/vfy?raw";
import { EXPLAIN_LOG_CHARS, EXPLAIN_MODEL, EXPLAIN_SYSTEM, explainMessages, isExplainChat, pickExplainModel, trimLog } from "./explain";

const numbered = (n: number) => Array.from({ length: n }, (_, i) => `line ${i + 1}`);

describe("trimLog", () => {
  it("keeps error lines with context and the last 30 lines, marking gaps", () => {
    const lines = numbered(100);
    lines[9] = "npm ERR! code ENOENT";
    const out = trimLog(lines).split("\n");
    // 2 lines before and 3 after the error, then a gap, then the tail.
    expect(out.slice(0, 8)).toEqual(["…", "line 8", "line 9", "npm ERR! code ENOENT", "line 11", "line 12", "line 13", "…"]);
    expect(out.slice(8)).toEqual(numbered(100).slice(70));
    expect(out).not.toContain("line 50");
  });

  it("returns a short log unchanged", () => {
    expect(trimLog(["a", "b", "c"])).toBe("a\nb\nc");
  });

  it("caps the size and keeps the tail", () => {
    const lines = Array.from({ length: 400 }, (_, i) => `error ${i} ${"x".repeat(40)}`);
    const out = trimLog(lines);
    expect(out.length).toBe(EXPLAIN_LOG_CHARS);
    expect(out.startsWith("…")).toBe(true);
    expect(out.endsWith(lines[399]!)).toBe(true);
  });
});

describe("explainMessages", () => {
  it("builds the prompt from the example failed build", () => {
    const [sys, user] = explainMessages(failedBuild, { app: "blog", commit: "4b1d000ff", commitMessage: "Add header\n\nmore" });
    expect(sys).toEqual({ role: "system", content: EXPLAIN_SYSTEM });
    expect(user!.role).toBe("user");
    expect(user!.content).toMatch(/^App: blog\nCommit: 4b1d000 "Add header"\n\nBuild log/);
    expect(user!.content).toContain("Module not found: Can't resolve '../components/Header'");
    expect(user!.content).toContain("Deployment failed.");
    expect(user!.content.trimEnd().endsWith("```")).toBe(true);
    expect(user!.content.length).toBeLessThan(EXPLAIN_LOG_CHARS + 300);
  });

  it("prefixes stderr lines and appends vercel.json", () => {
    const [, user] = explainMessages(
      [
        { output: "next build", type: "stdout" },
        { output: "Error: boom", type: "stderr" },
      ],
      { vercelJson: '{"cleanUrls": true}' },
    );
    expect(user!.content).toContain("next build\n[stderr] Error: boom");
    expect(user!.content).toContain('vercel.json:\n```json\n{"cleanUrls": true}\n```');
    expect(user!.content.startsWith("Build log")).toBe(true);
  });

  it("is recognised as the deploy-doctor chat", () => {
    expect(isExplainChat(explainMessages("Error: boom"))).toBe(true);
    expect(isExplainChat([{ role: "user", content: "hi" }])).toBe(false);
  });
});

describe("pickExplainModel", () => {
  it("prefers Gemma 4 E2B, then any Gemma 4, then a chat model", () => {
    expect(pickExplainModel(["llama3.2:3b", "gemma4:e4b", EXPLAIN_MODEL])).toBe(EXPLAIN_MODEL);
    expect(pickExplainModel(["nomic-embed-text:latest", "llama3.2:3b", "gemma4:e4b"])).toBe("gemma4:e4b");
    expect(pickExplainModel(["nomic-embed-text:latest", "llama3.2:3b"])).toBe("llama3.2:3b");
    expect(pickExplainModel(["nomic-embed-text:latest"])).toBeUndefined();
    expect(pickExplainModel([])).toBeUndefined();
  });
});

describe("cli/vfy", () => {
  it("sends the same system prompt as the cockpit", () => {
    const m = vfy.match(/^EXPLAIN_SYSTEM="([\s\S]*?)"$/m);
    expect(m).not.toBeNull();
    for (const line of EXPLAIN_SYSTEM.split("\n")) expect(m![1]!.split("\n")).toContain(line);
    expect(m![1]).toBe(EXPLAIN_SYSTEM);
    expect(vfy).toContain(`AI_MODEL=\${VFY_AI_MODEL:-${EXPLAIN_MODEL}}`);
  });
});
