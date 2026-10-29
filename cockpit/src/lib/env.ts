/** One KEY=value pair from a .env file. */
export interface EnvVar {
  key: string;
  value: string;
}

/** What the agent accepts as a variable name. */
export const ENV_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * Read a .env file with the agent's rules (`env::parse_dotenv`), so the
 * import dialog shows what the agent will save, or the line it will refuse:
 * comments, blank lines, `export KEY=…`, single, double and backtick quotes
 * (double quotes expand \n \r \t \" \\ \$), quoted values over several lines
 * (PEM keys), a `# comment` after a value. Later duplicates win.
 */
export function readDotenv(text: string): { vars: EnvVar[]; error?: string } {
  const out = new Map<string, string>();
  const lines = text.replace(/^﻿/, "").replace(/\r\n?/g, "\n").split("\n");
  let i = 0;
  while (i < lines.length) {
    const n = i + 1;
    let line = lines[i++]!.trim();
    if (!line || line.startsWith("#")) continue;
    const fail = (what: string) => ({ vars: [...out].map(([key, value]) => ({ key, value })), error: `line ${n}: ${what}` });
    if (/^export[ \t]/.test(line)) line = line.slice(7);
    const eq = line.indexOf("=");
    if (eq < 0) return fail("expected KEY=value");
    const key = line.slice(0, eq).trim();
    if (!ENV_KEY.test(key) || key.length > 256) return fail(`\`${key}\` is not a variable name`);
    const v = line.slice(eq + 1).trimStart();
    const q = v[0];
    if (q === '"' || q === "'" || q === "`") {
      let body = v.slice(1);
      let close = closingQuote(body, q);
      while (close < 0) {
        if (i >= lines.length) return fail(`the ${q} quote is never closed`);
        body += "\n" + lines[i++];
        close = closingQuote(body, q);
      }
      const rest = body.slice(close + 1).trim();
      if (rest && !rest.startsWith("#")) return fail(`unexpected \`${rest}\` after the closing quote`);
      const inner = body.slice(0, close);
      out.set(key, q === '"' ? unescape(inner) : inner);
    } else {
      out.set(key, v.replace(/(^|[ \t])#.*$/, "").trim());
    }
  }
  return { vars: [...out].map(([key, value]) => ({ key, value })) };
}

function closingQuote(s: string, q: string): number {
  let escaped = false;
  for (let at = 0; at < s.length; at++) {
    const c = s[at];
    if (escaped) escaped = false;
    else if (c === "\\" && q === '"') escaped = true;
    else if (c === q) return at;
  }
  return -1;
}

const ESCAPES: Record<string, string> = { n: "\n", r: "\r", t: "\t", '"': '"', "\\": "\\", $: "$" };

function unescape(s: string): string {
  return s.replace(/\\([\s\S]?)/g, (m, c: string) => ESCAPES[c] ?? m);
}
