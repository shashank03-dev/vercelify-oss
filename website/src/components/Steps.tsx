import { Card, CopyCommand, INSTALL_HOST, INSTALL_MAC, Reveal, Section, SectionHeader, Shot } from "./ui";

const STEPS = [
  {
    n: "01",
    title: "Run one line on the laptop",
    body: "The installer sets up Docker, Vercelify's engine and private network, the firewall and power management, then prints a pairing code.",
    cmd: INSTALL_HOST,
    display: "curl -fsSL …/install.sh | sudo bash",
  },
  {
    n: "02",
    title: "Install the Mac app",
    body: "One more line installs the Vercelify app and its network daemon. No account to create, nothing to sign in to.",
    cmd: INSTALL_MAC,
    display: "curl -fsSL …/install-mac.sh | bash",
  },
  {
    n: "03",
    title: "Type the pairing code",
    body: "Pick your laptop in the app and enter its 8-character code. Your dashboard opens, ready for the first app.",
  },
];

export function Steps() {
  return (
    <Section id="install" className="pb-20 sm:pb-32">
      <div className="relative overflow-hidden rounded-3xl border border-line bg-panel/40">
        <div aria-hidden className="dot-grid absolute inset-0 [mask-image:linear-gradient(to_bottom,#000,transparent_70%)]" />
        <div className="relative grid gap-12 p-4 [&>*]:min-w-0 sm:p-10 lg:grid-cols-[1fr_1.05fr] lg:gap-16 lg:p-14">
          <div>
            <SectionHeader
              eyebrow="How it works"
              size="md"
              title="Online in three steps."
              muted="About 30 minutes, mostly downloads."
            />
            <ol className="mt-10 space-y-3">
              {STEPS.map((s, i) => (
                <Reveal key={s.n} delay={i * 0.08}>
                  <li className="rounded-xl border border-line bg-surface/70 p-4 sm:p-5">
                    {/* The number sits above the title on phones, so the command gets the card's full width. */}
                    <div className="flex flex-col gap-1.5 sm:flex-row sm:items-baseline sm:gap-4">
                      <span className="font-mono text-[12px] text-accent">{s.n}</span>
                      <div className="min-w-0 flex-1">
                        <h3 className="text-[16px] font-medium">{s.title}</h3>
                        <p className="mt-1.5 text-[14.5px] leading-relaxed text-fg-muted">{s.body}</p>
                        {s.cmd && <CopyCommand className="mt-4" command={s.cmd} display={s.display} />}
                      </div>
                    </div>
                  </li>
                </Reveal>
              ))}
            </ol>
            <p className="mt-6 text-[14px] text-fg-dim">
              No domain, credit card or router settings needed. Any 64-bit PC with 4 GB of RAM works; 8 GB is comfortable.
            </p>
          </div>
          <Reveal delay={0.15} className="min-w-0 lg:pt-24">
            <Terminal />
            <Pairing />
          </Reveal>
        </div>
      </div>
    </Section>
  );
}

function Terminal() {
  return (
    <Card className="bg-[#0d0d0c]">
      <div className="flex items-center gap-1.5 border-b border-line px-4 py-3">
        <span className="size-2.5 rounded-full bg-surface-3" />
        <span className="size-2.5 rounded-full bg-surface-3" />
        <span className="size-2.5 rounded-full bg-surface-3" />
        <span className="ml-3 font-mono text-[11px] text-fg-dim">thinkpad-t470 — install.sh</span>
      </div>
      <pre className="overflow-x-auto px-4 py-4 font-mono text-[11.5px] leading-[1.75] text-fg-muted sm:px-5 sm:py-5 sm:text-[12.5px]">
        <span className="text-ok">✔</span> Docker 28 and the Vercelify agent installed{"\n"}
        <span className="text-ok">✔</span> this server coordinates the mesh as 10.77.0.1{"\n"}
        <span className="text-ok">✔</span> Firewall, lid-close and battery guard on{"\n"}
        {"\n"}
        <span className="font-semibold text-fg">✔ Vercelify is ready</span>
        {"\n\n"}
        {"  "}Connect your Mac{"\n"}
        {"    "}Open Vercelify, pick <span className="text-fg">thinkpad-t470</span>, type{"\n"}
        {"    "}the code <span className="text-accent-fg">K7QF-M2XD</span> <span className="text-fg-dim">(valid 1 hour)</span>
      </pre>
    </Card>
  );
}

/** Step three, as it really looks: the app found the laptop and takes its code. */
function Pairing() {
  return (
    <Shot
      src="/app/pairing.jpg"
      alt="The Vercelify app pairing with a laptop found on the tailnet using an 8-character code"
      pos="87% 64%"
      zoom={2.2}
      className="mt-4 aspect-[16/9] shadow-[0_30px_80px_-30px_rgb(0_0_0/0.8)]"
    />
  );
}
