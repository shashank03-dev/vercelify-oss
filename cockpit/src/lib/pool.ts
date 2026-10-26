// Adding a machine: the install command, with the main server's mesh
// invite for a worker (it joins and runs apps for that main server).

/** The install command for a new machine with the chosen options. */
export function installCommand(opts: { worker: boolean; hostname?: string; ai?: boolean; join?: string }): string {
  const args: string[] = [];
  if (opts.worker) args.push("--worker");
  if (opts.worker && opts.join) args.push("--join", opts.join);
  const h = (opts.hostname ?? "").trim().toLowerCase();
  if (h) args.push("--hostname", h);
  if (opts.ai) args.push("--ai");
  const base = "curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash";
  return args.length ? `${base} -s -- ${args.join(" ")}` : base;
}

/** Same rule as the installer's --hostname check. */
export const validHostname = (h: string) => h === "" || /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(h);
