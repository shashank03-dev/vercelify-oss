import vectors from "../../../crates/vercelify-core/tests/fixtures/volume-vectors.json";
import { composeSnippet, PRESETS, resolveVolume, VolumeError } from "./compose";
import type { StorageTier } from "../types";

interface Vector {
  tier: StorageTier;
  service: string;
  name: string | null;
  source?: string;
  host_path?: string;
  error?: "service" | "name";
}

describe("volume rules match the Rust implementation", () => {
  for (const v of vectors as Vector[]) {
    it(`${v.tier} ${v.service} ${v.name ?? "(none)"}`, () => {
      if (v.error) {
        expect(() => resolveVolume(v.tier, v.service, v.name)).toThrow(VolumeError);
        try {
          resolveVolume(v.tier, v.service, v.name);
        } catch (e) {
          expect((e as VolumeError).kind).toBe(v.error);
        }
      } else {
        expect(resolveVolume(v.tier, v.service, v.name)).toEqual({ source: v.source, hostPath: v.host_path });
      }
    });
  }
});

describe("composeSnippet", () => {
  it("HDD bind mount", () => {
    expect(composeSnippet({ service: "postgres", name: "data", tier: "hdd", containerPath: "/var/lib/postgresql/data", image: "postgres:17" }))
      .toBe(`services:
  postgres:
    image: postgres:17
    volumes:
      - /mnt/storage/volumes/postgres/data:/var/lib/postgresql/data
`);
  });

  it("HDD named volume backed by a bind", () => {
    expect(composeSnippet({ service: "minio", name: "objects", tier: "hdd", containerPath: "/data", style: "named" })).toBe(`services:
  minio:
    volumes:
      - minio-objects:/data

volumes:
  minio-objects:
    driver: local
    driver_opts:
      type: none
      o: bind
      device: /mnt/storage/volumes/minio/objects
`);
  });

  it("SSD named volume", () => {
    expect(composeSnippet({ service: "redis", name: "data", tier: "ssd", containerPath: "/data" })).toBe(`services:
  redis:
    volumes:
      - redis-data:/data

volumes:
  redis-data: {}
`);
  });

  it("rejects relative or escaping container paths", () => {
    expect(() => composeSnippet({ service: "x", tier: "ssd", containerPath: "data" })).toThrow();
    expect(() => composeSnippet({ service: "x", tier: "ssd", containerPath: "/a/../etc" })).toThrow();
  });

  it("every preset produces a valid snippet on both tiers", () => {
    for (const p of PRESETS) {
      for (const tier of ["ssd", "hdd"] as const) {
        expect(composeSnippet({ service: p.id, name: p.name, tier, containerPath: p.containerPath })).toContain(p.containerPath);
      }
    }
  });
});
