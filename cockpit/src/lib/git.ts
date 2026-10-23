// Git URLs as Coolify stores them, turned into something `git ls-remote` takes.

/**
 * Coolify keeps GitHub repositories as `owner/repo` (GitHub App sources) or a
 * full URL. Returns an https clone URL, or null when it can't tell.
 */
export function cloneUrl(repo: string | null | undefined): string | null {
  const r = (repo ?? "").trim();
  if (!r) return null;
  if (/^https?:\/\//.test(r)) return r.replace(/\/+$/, "");
  const ssh = /^git@([^:]+):(.+?)(\.git)?$/.exec(r);
  if (ssh) return `https://${ssh[1]}/${ssh[2]}.git`;
  if (/^[\w.-]+\/[\w.-]+$/.test(r)) return `https://github.com/${r.replace(/\.git$/, "")}.git`;
  return null;
}

/** "https://github.com/you/my-app.git" -> "my-app" */
export function repoName(url: string): string {
  const last = url.replace(/\/+$/, "").split("/").pop() ?? "";
  return last.replace(/\.git$/, "").toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "") || "app";
}

/** Same subset of git's ref-name rules the agent enforces. */
export function validBranch(b: string): boolean {
  return (
    b.length > 0 &&
    b.length <= 200 &&
    /^[A-Za-z0-9._/-]+$/.test(b) &&
    !/^[-/.]/.test(b) &&
    !/[/.]$/.test(b) &&
    !b.endsWith(".lock") &&
    !b.includes("..") &&
    !b.includes("//")
  );
}
