import { lazy, Suspense, useEffect, useState } from "react";
import { weakDevice } from "./ui";

// WebGPU only: the component and vgpu are fetched on demand, so phones, old
// browsers and reduced-motion visitors never download them.
const AeroShards = lazy(() => import("./AeroShards/AeroShards.jsx"));

function canRun() {
  if (typeof window === "undefined" || !("gpu" in navigator)) return false;
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches || weakDevice()) return false;
  return window.matchMedia("(min-width: 900px) and (pointer: fine)").matches;
}

/**
 * A ribbon of ember glass drifting through the right of the hero, behind the
 * text. It fades in once the GPU is ready and removes itself on any error, so
 * the glyph field alone is the fallback.
 */
export function HeroShards() {
  const [on, setOn] = useState(false);
  const [shown, setShown] = useState(false);
  useEffect(() => setOn(canRun()), []);
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
          placement="right"
          material="glass"
          shardColor="#c4745a"
          accentColor="#ff6a3d"
          backgroundColor="transparent"
          count={700}
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
          dpr={1.5}
          onError={(e) => {
            if (import.meta.env.DEV) console.warn("AeroShards disabled:", e.message);
            setOn(false);
          }}
        />
      </Suspense>
    </div>
  );
}
