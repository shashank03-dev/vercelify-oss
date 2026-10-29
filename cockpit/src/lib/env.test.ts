import { readDotenv } from "./env";

describe("readDotenv", () => {
  it("reads plain, exported, quoted and commented lines", () => {
    const { vars, error } = readDotenv(
      [
        "# database",
        "DATABASE_URL=postgres://u:p@db:5432/app",
        "export NODE_ENV=production",
        "",
        "API_KEY = 'sk-123#not-a-comment'",
        'GREETING="hello\\nworld"',
        "PORT=3000 # web",
        "URL=http://x/#frag",
      ].join("\n"),
    );
    expect(error).toBeUndefined();
    expect(vars).toEqual([
      { key: "DATABASE_URL", value: "postgres://u:p@db:5432/app" },
      { key: "NODE_ENV", value: "production" },
      { key: "API_KEY", value: "sk-123#not-a-comment" },
      { key: "GREETING", value: "hello\nworld" },
      { key: "PORT", value: "3000" },
      { key: "URL", value: "http://x/#frag" },
    ]);
  });

  it("joins multi-line quoted values and lets later keys win", () => {
    const { vars } = readDotenv('﻿KEY="-----BEGIN\nabc\n-----END"\r\nA=1\nA=2\nEMPTY=');
    expect(vars).toEqual([
      { key: "KEY", value: "-----BEGIN\nabc\n-----END" },
      { key: "A", value: "2" },
      { key: "EMPTY", value: "" },
    ]);
  });

  it("keeps the next lines after a comment that follows a quoted value", () => {
    const { vars, error } = readDotenv('Q="v" # note\nB=2\nS="a\\\\nb \\"q\\""');
    expect(error).toBeUndefined();
    expect(vars).toEqual([
      { key: "Q", value: "v" },
      { key: "B", value: "2" },
      { key: "S", value: 'a\\nb "q"' },
    ]);
  });

  it("names the line the agent would refuse", () => {
    expect(readDotenv("A=1\nnot a variable").error).toBe("line 2: expected KEY=value");
    expect(readDotenv("1BAD=x").error).toMatch(/^line 1: /);
    expect(readDotenv("A.B=x").error).toMatch(/^line 1: /);
    expect(readDotenv('A="open\nB=1').error).toBe('line 1: the " quote is never closed');
    expect(readDotenv('A="v" junk').error).toMatch(/after the closing quote/);
  });
});
