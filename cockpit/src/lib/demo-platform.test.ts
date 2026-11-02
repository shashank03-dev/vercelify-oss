import { describe, expect, it } from "vitest";
import { platformRoute } from "./demo-platform";
import type { EnvProvenance, EnvVar } from "../types";

const call = <T>(method: "GET" | "PUT", path: string, body?: unknown) => platformRoute({ method, path: `/v1/platform${path}`, body }) as T;

describe("demo previews get only what is shared (design §5.1)", () => {
  it("withholds production values, blocks shared database references and never sends values", () => {
    let p = call<EnvProvenance>("GET", "/apps/shop/env/provenance");
    expect(p.withheld.map((w) => [w.key, w.reason])).toEqual([
      ["DATABASE_URL", "production_only"],
      ["STRIPE_SECRET_KEY", "production_only"],
    ]);
    expect(p.preview.map((v) => [v.key, v.origin])).toEqual([["NEXT_PUBLIC_SITE_URL", "shared"]]);
    expect(JSON.stringify(p)).not.toContain("sk_live");

    call("PUT", "/apps/shop/env/DATABASE_URL/share", { share: true });
    p = call<EnvProvenance>("GET", "/apps/shop/env/provenance");
    expect(p.withheld.find((w) => w.key === "DATABASE_URL")?.reason).toBe("production_database");

    // A preview-only value takes over for previews; production keeps its own.
    call("PUT", "/apps/shop/env", { vars: [{ key: "DATABASE_URL", value: "postgres://preview", preview_only: true }], replace: false });
    p = call<EnvProvenance>("GET", "/apps/shop/env/provenance");
    expect(p.withheld.some((w) => w.key === "DATABASE_URL")).toBe(false);
    expect(p.preview.find((v) => v.key === "DATABASE_URL")?.origin).toBe("preview_only");
    const vars = call<EnvVar[]>("GET", "/apps/shop/env");
    expect(vars.filter((v) => v.key === "DATABASE_URL").map((v) => v.preview_only)).toEqual([false, true]);

    call("PUT", "/apps/shop/env", { vars: [{ key: "DATABASE_URL", value: "${{db.production.url}}", preview_only: true }], replace: false });
    p = call<EnvProvenance>("GET", "/apps/shop/env/provenance");
    expect(p.withheld.find((w) => w.key === "DATABASE_URL")?.reason).toBe("production_database");
    expect(p.preview.some((v) => v.key === "DATABASE_URL")).toBe(false);
  });
});
