import { motion } from "framer-motion";
import { BrainCircuit, Check, GitBranch, Globe, Laptop, ShieldCheck } from "lucide-react";
import type { ReactNode } from "react";
import { HOST_INSTALL, PairForm } from "../components/PairForm";
import { CodeBlock, InlineCode, Logo } from "../components/ui";
import type { CockpitSettings } from "../types";

const FEATURES = [
  { icon: GitBranch, text: "Push to a branch and it deploys, with a preview URL for every pull request" },
  { icon: BrainCircuit, text: "Run local AI models on your old laptops and use them from this Mac" },
  { icon: Laptop, text: "Connect several old machines and switch between them in one click" },
  { icon: Globe, text: "Your own domains with automatic HTTPS, even behind CGNAT" },
  { icon: ShieldCheck, text: "Private by default on your own encrypted network, no open ports" },
];

export function Onboarding({ onPaired }: { onPaired: (s: CockpitSettings, note?: string) => void }) {
  return (
    <div className="flex h-full">
      <div data-tauri-drag-region className="fixed inset-x-0 top-0 z-10 h-11" />

      {/* Brand side */}
      <aside className="dot-grid relative hidden w-[44%] max-w-[560px] flex-col justify-between overflow-hidden border-r border-line bg-sidebar p-10 pt-16 lg:flex">
        <div
          aria-hidden
          className="pointer-events-none absolute -bottom-40 -left-24 size-[520px] rounded-full opacity-60 blur-3xl"
          style={{ background: "radial-gradient(closest-side, color-mix(in srgb, var(--accent) 22%, transparent), transparent)" }}
        />
        <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.4 }} className="relative">
          <div className="flex items-center gap-2.5">
            <Logo className="h-6 w-auto" />
            <span className="text-[17px] font-semibold tracking-[-0.02em]">Vercelify</span>
          </div>
          <h2 className="mt-14 text-[34px] leading-[1.08] font-semibold tracking-[-0.035em]">
            Your own Vercel,
            <br />
            <span className="text-brand">on the laptop in your drawer.</span>
          </h2>
          <p className="mt-4 max-w-sm text-[14px] leading-relaxed text-fg-muted">
            Turn an old PC or laptop into a free, private cloud for all your apps. No monthly bill, no lock-in.
          </p>
        </motion.div>
        <ul className="relative space-y-3">
          {FEATURES.map((f, i) => (
            <motion.li
              key={f.text}
              initial={{ opacity: 0, x: -6 }}
              animate={{ opacity: 1, x: 0 }}
              transition={{ delay: 0.15 + i * 0.06 }}
              className="flex items-center gap-3 text-[13px] text-fg-muted"
            >
              <span className="flex size-7 shrink-0 items-center justify-center rounded-lg border border-line-strong bg-surface">
                <f.icon className="size-3.5 text-fg" />
              </span>
              {f.text}
            </motion.li>
          ))}
        </ul>
      </aside>

      {/* Setup side */}
      <main className="flex min-w-0 flex-1 items-center justify-center overflow-auto px-8 py-16">
        <motion.div initial={{ opacity: 0, y: 12 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.35 }} className="w-full max-w-[520px]">
          <div className="mb-8">
            <Logo className="mb-5 h-7 w-auto lg:hidden" />
            <h1 className="text-[24px] font-semibold tracking-[-0.025em]">Welcome to Vercelify</h1>
            <p className="mt-1.5 text-[13px] text-fg-muted">Two steps and your machine is ready to take deployments.</p>
          </div>

          <ol>
            <Step n={1} title="Run the installer on your Linux machine">
              <CodeBlock code={HOST_INSTALL} prompt />
              <p className="mt-2 text-xs text-fg-dim">
                Debian 12/13 or Ubuntu 22.04/24.04. It sets up Docker, Vercelify's engine, its private network, storage and the agent, then
                shows an 8-character code.
              </p>
            </Step>
            <Step n={2} title="Connect" last>
              <p className="mb-3 text-[13px] text-fg-muted">
                On the same Wi-Fi the server shows up below: type its code and this computer joins its private network. Anywhere else, paste
                the full code that <InlineCode>sudo vercelify pair</InlineCode> prints.
              </p>
              <PairForm onPaired={onPaired} />
            </Step>
          </ol>
        </motion.div>
      </main>
    </div>
  );
}

function Step({ n, title, children, last, done }: { n: number; title: string; children: ReactNode; last?: boolean; done?: boolean }) {
  return (
    <li className="relative flex gap-4 pb-7">
      {!last && <span className="absolute top-8 bottom-1 left-[13px] w-px bg-line-strong" />}
      <span className="relative flex size-7 shrink-0 items-center justify-center rounded-full border border-line-strong bg-surface-2 text-xs font-semibold tabular-nums">
        {done ? <Check className="size-3.5 text-ok" /> : n}
      </span>
      <div className="min-w-0 flex-1 pt-1">
        <div className="mb-2 text-[14px] font-medium">{title}</div>
        {children}
      </div>
    </li>
  );
}
