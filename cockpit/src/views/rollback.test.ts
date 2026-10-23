import { rollbackTargets } from "./AppsView";
import type { CoolifyDeployment } from "../types";

const d = (commit: string, pr = 0, status = "finished"): CoolifyDeployment =>
  ({ deployment_uuid: `${commit}-${pr}-${Math.random()}`, commit, pull_request_id: pr, status }) as CoolifyDeployment;

describe("rollbackTargets", () => {
  it("never offers the live commit and offers each older commit once", () => {
    const r = rollbackTargets([d("pr42", 42), d("v3"), d("v3"), d("v2"), d("v2"), d("vX", 0, "failed"), d("v1")]);
    expect(r.map((x) => x.canRollback)).toEqual([false, false, false, true, false, false, true]);
  });

  it("a newer PR deployment does not count as live", () => {
    const r = rollbackTargets([d("pr", 7), d("v2"), d("v1")]);
    expect(r.map((x) => x.canRollback)).toEqual([false, false, true]);
  });
});

describe("rollbackTargets live marker", () => {
  it("marks only the newest finished production deployment as current", () => {
    const r = rollbackTargets([d("pr", 7), d("vX", 0, "failed"), d("v2"), d("v2"), d("v1")]);
    expect(r.map((x) => x.live)).toEqual([false, false, true, false, false]);
  });
});
