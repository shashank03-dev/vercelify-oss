import { lazy, Suspense, useEffect, useState } from "react";

// Fetched on demand. AeroShards draws with WebGPU where the browser has it and
// with WebGL2 everywhere else, so every current browser shows the shards.
const AeroShards = lazy(() => import("./AeroShards/AeroShards.jsx"));

function canRun() {
  if (typeof window === "undefined") return false;
  if ("gpu" in navigator) return true;
  try {
    return !!document.createElement("canvas").getContext("webgl2");
  } catch {
    return false;
  }
}

// Phones and tablets: fewer shards at 1x, since every shard is ray traced.
const compact = () => window.matchMedia("(max-width: 899px), (pointer: coarse)").matches;

// A lost GPU context (a driver reset, a backgrounded phone tab) gets this many
// fresh starts before the shards give way to the glyph field.
const RETRIES = 2;

/**
 * A ribbon of ember glass drifting through the right of the hero, behind the
 * text. It fades in once the GPU is ready; on an error it starts again, and
 * after RETRIES it removes itself, so the glyph field alone is the fallback.
 */
export function HeroShards() {
  const [on, setOn] = useState(false);
  const [shown, setShown] = useState(false);
  const [small, setSmall] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    setSmall(compact());
    setOn(canRun());
  }, []);
  useEffect(() => {
    if (!on) return;
    const t = window.setTimeout(() => setShown(true), 250);
    return () => window.clearTimeout(t);
  }, [on]);
  if (!on) return null;

  return (
    <div
      aria-hidden
      className="pointer-events-none absolute top-0 right-0 h-[780px] w-[62%] transition-opacity duration-[1600ms] [mask-image:linear-gradient(90deg,transparent,#000_30%),linear-gradient(to_bottom,#000_65%,transparent)] [mask-composite:intersect]"
      style={{ opacity: shown ? 1 : 0 }}
    >
      <Suspense fallback={null}>
        <AeroShards
          key={attempt}
          placement="right"
          material="glass"
          shardColor="#c4745a"
          accentColor="#ff6a3d"
          backgroundColor="transparent"
          count={small ? 320 : 700}
          shardSize={0.9}
          speed={0.6}
          glow={0.45}
          grain={0.04}
          vignette={0.25}
          depthOfField={0.7}
          markers="plus"
          mouseStrength={0.45}
          parallax={0.35}
          holdToGather={false}
          dpr={small ? 1 : 1.5}
          onError={(e) => {
            if (import.meta.env.DEV) console.warn("AeroShards:", e.message);
            if (attempt < RETRIES) window.setTimeout(() => setAttempt((n) => n + 1), 400);
            else setOn(false);
          }}
        />
      </Suspense>
    </div>
  );
}
