import { parseCallback } from "./GitIntegrations";

describe("GitHub App callback addresses", () => {
  it("finds the code and state GitHub put in the address", () => {
    expect(parseCallback(" http://192.168.1.20:7443/v1/github-app/callback?code=abc123&state=XyZ ")).toEqual({ code: "abc123", state: "XyZ" });
    expect(parseCallback("https://mesh.example.com/v1/github-app/callback?state=s&code=c")).toEqual({ code: "c", state: "s" });
  });

  it("rejects what isn't one", () => {
    expect(parseCallback("abc123")).toBeNull();
    expect(parseCallback("https://github.com/settings/apps")).toBeNull();
    expect(parseCallback("https://x/cb?code=only")).toBeNull();
  });
});
