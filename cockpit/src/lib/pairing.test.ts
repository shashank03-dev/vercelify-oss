import fixture from "../../../crates/vercelify-core/tests/fixtures/pairing-code.txt?raw";
import { decodePairing, encodePairing } from "./pairing";

describe("pairing codes", () => {
  it("decodes a code produced by `vercelify-agent pair`", () => {
    const p = decodePairing(fixture);
    expect(p).toEqual({
      agent_url: "http://vercelify-host.tail1234.ts.net:7420",
      agent_token: "0123456789abcdef0123456789abcdef",
      coolify_url: "http://vercelify-host.tail1234.ts.net:8000",
      coolify_token: "1|coolifytoken",
      hostname: "vercelify-host",
    });
  });

  it("round-trips and tolerates copy/paste whitespace", () => {
    const p = decodePairing(fixture);
    const code = encodePairing(p);
    expect(code).toBe(fixture.trim());
    expect(decodePairing(`  ${code.slice(0, 30)}\n${code.slice(30)}  `)).toEqual(p);
  });

  it("rejects garbage with helpful messages", () => {
    expect(() => decodePairing("hello")).toThrow(/must start with 'vfy1\.'/);
    expect(() => decodePairing("vfy1.!!!")).toThrow(/base64/);
    expect(() => decodePairing(encodePairing({ agent_url: "ftp://x", agent_token: "0123456789abcdef", coolify_url: "", hostname: "" }))).toThrow(
      /agent_url/,
    );
  });
});
