import { parseDotenv } from "./env";

describe("parseDotenv", () => {
  it("reads plain, exported, quoted and commented lines", () => {
    const vars = parseDotenv(
      [
        "# database",
        "DATABASE_URL=postgres://u:p@db:5432/app",
        "export NODE_ENV=production",
        "",
        "API_KEY = 'sk-123#not-a-comment'",
        'GREETING="hello\\nworld"',
        "PORT=3000 # web",
        "not a variable",
        "1BAD=x",
      ].join("\n"),
    );
    expect(vars).toEqual([
      { key: "DATABASE_URL", value: "postgres://u:p@db:5432/app" },
      { key: "NODE_ENV", value: "production" },
      { key: "API_KEY", value: "sk-123#not-a-comment" },
      { key: "GREETING", value: "hello\nworld" },
      { key: "PORT", value: "3000" },
    ]);
  });

  it("joins multi-line double-quoted values and lets later keys win", () => {
    const vars = parseDotenv('KEY="-----BEGIN\nabc\n-----END"\r\nA=1\nA=2\nEMPTY=');
    expect(vars).toEqual([
      { key: "KEY", value: "-----BEGIN\nabc\n-----END" },
      { key: "A", value: "2" },
      { key: "EMPTY", value: "" },
    ]);
  });
});
