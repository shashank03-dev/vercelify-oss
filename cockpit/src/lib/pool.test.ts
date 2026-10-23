import { installCommand, isIpv4, validHostname } from "./pool";

describe("adding machines", () => {
  it("builds the install command from the wizard's choices", () => {
    const base = "curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash";
    expect(installCommand({ worker: false })).toBe(base);
    expect(installCommand({ worker: true })).toBe(`${base} -s -- --worker`);
    expect(installCommand({ worker: true, hostname: " Old-Dell ", ai: true })).toBe(`${base} -s -- --worker --hostname old-dell --ai`);
  });

  it("validates hostnames like the installer", () => {
    expect(validHostname("")).toBe(true);
    expect(validHostname("thinkpad-t470")).toBe(true);
    expect(validHostname("-bad")).toBe(false);
    expect(validHostname("has space")).toBe(false);
    expect(validHostname("UPPER")).toBe(false);
  });

  it("recognises IPv4", () => {
    expect(isIpv4("100.64.0.2")).toBe(true);
    expect(isIpv4("fd7a:115c::1")).toBe(false);
  });
});
