import { describe, expect, it } from "vitest";
import { browserUrl } from "./browser-url";
import capability from "../../src-tauri/capabilities/default.json";

describe("private browser URLs", () => {
  it("maps app, preview and private domain HTTP URLs in app mode", () => {
    for (const host of ["shop.10.77.0.1.sslip.io", "pr-3.shop.main.vfy", "blog.example.com"]) {
      expect(browserUrl(`http://${host}/a?q=1#b`, true)).toBe(`http://${host}.localhost:7481/a?q=1#b`);
    }
  });
  it("preserves public HTTPS, system mode, and already mapped URLs", () => {
    expect(browserUrl("https://shop.example.com/x", true)).toBe("https://shop.example.com/x");
    expect(browserUrl("http://shop.main.vfy/", false)).toBe("http://shop.main.vfy/");
    const gateway = "http://shop.main.vfy.localhost:7481/x";
    expect(browserUrl(gateway, true)).toBe(gateway);
  });
  it("uses HTTP to the gateway for mesh HTTPS and refuses to map unrelated schemes", () => {
    expect(browserUrl("https://shop.main.vfy/x", true)).toBe("http://shop.main.vfy.localhost:7481/x");
    expect(browserUrl("mailto:a@b", true)).toBe("mailto:a@b");
    expect(browserUrl("broken", true)).toBe("broken");
  });
  it("scopes the desktop opener to browser URLs", () => {
    expect(capability.permissions).toContainEqual({ identifier: "opener:allow-open-url", allow: [{ url: "http://*" }, { url: "https://*" }, { url: "http://*.localhost:7481/*" }] });
  });
});
