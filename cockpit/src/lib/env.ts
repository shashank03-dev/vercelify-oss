/** One KEY=value pair from a .env file. */
export interface EnvVar {
  key: string;
  value: string;
}

const KEY = /^[A-Za-z_][A-Za-z0-9_.-]*$/;

/**
 * Parse a .env file the way dotenv does for the common cases: comments,
 * blank lines, `export KEY=…`, single/double quotes (double quotes expand
 * \n), multi-line double-quoted values and trailing `# comments` on unquoted
 * values. Later duplicates win. Lines that aren't KEY=value are skipped.
 */
export function parseDotenv(text: string): EnvVar[] {
  const out = new Map<string, string>();
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!.trim();
    if (!line || line.startsWith("#")) continue;
    const m = /^(?:export\s+)?([^=\s]+)\s*=\s*(.*)$/.exec(line);
    if (!m || !KEY.test(m[1]!)) continue;
    const key = m[1]!;
    let raw = m[2]!;
    const q = raw[0];
    if (q === '"' || q === "'" || q === "`") {
      // Collect lines until the closing quote (multi-line values).
      let body = raw.slice(1);
      while (!body.trimEnd().endsWith(q) && i + 1 < lines.length) body += "\n" + lines[++i];
      body = body.trimEnd();
      raw = body.endsWith(q) ? body.slice(0, -1) : body;
      out.set(key, q === '"' ? raw.replace(/\\n/g, "\n").replace(/\\"/g, '"') : raw);
    } else {
      out.set(key, raw.replace(/\s+#.*$/, "").trim());
    }
  }
  return [...out].map(([key, value]) => ({ key, value }));
}
