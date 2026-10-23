import { cloneUrl, repoName, validBranch } from "./git";

describe("git helpers", () => {
  it("turns Coolify repository fields into clone URLs", () => {
    expect(cloneUrl("shashank03-dev/portfolio")).toBe("https://github.com/shashank03-dev/portfolio.git");
    expect(cloneUrl("https://github.com/you/app")).toBe("https://github.com/you/app");
    expect(cloneUrl("https://gitlab.com/group/app.git/")).toBe("https://gitlab.com/group/app.git");
    expect(cloneUrl("git@github.com:you/app.git")).toBe("https://github.com/you/app.git");
    expect(cloneUrl("")).toBeNull();
    expect(cloneUrl(null)).toBeNull();
    expect(cloneUrl("not a repo")).toBeNull();
  });

  it("derives app names", () => {
    expect(repoName("https://github.com/you/My_App.git")).toBe("my-app");
    expect(repoName("https://github.com/you/")).toBe("you");
  });

  it("validates branches like the agent", () => {
    for (const ok of ["main", "release/v1.2", "feat_x-1"]) expect(validBranch(ok)).toBe(true);
    for (const bad of ["", "-x", "a..b", "a/", "x.lock", "a b", "/main"]) expect(validBranch(bad)).toBe(false);
  });
});
