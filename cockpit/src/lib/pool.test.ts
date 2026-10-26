import { installCommand, validHostname } from "./pool";

describe("adding machines", () => {
  it("builds the install command from the wizard's choices", () => {
    const base = "curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash";
    expect(installCommand({ worker: false })).toBe(base);
    expect(installCommand({ worker: true })).toBe(`${base} -s -- --worker`);
    expect(installCommand({ worker: true, hostname: " Old-Dell ", ai: true })).toBe(`${base} -s -- --worker --hostname old-dell --ai`);
    // A worker joins its main server's network with the invite it carries.
    expect(installCommand({ worker: true, join: "vfyj1.abc" })).toBe(`${base} -s -- --worker --join vfyj1.abc`);
    expect(installCommand({ worker: false, join: "vfyj1.abc" })).toBe(base);
  });

  it("validates hostnames like the installer", () => {
    expect(validHostname("")).toBe(true);
    expect(validHostname("thinkpad-t470")).toBe(true);
    expect(validHostname("-bad")).toBe(false);
    expect(validHostname("has space")).toBe(false);
    expect(validHostname("UPPER")).toBe(false);
  });
});
