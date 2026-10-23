import { lazy, type RefObject, Suspense, useEffect, useRef, useState } from "react";
import type { LaserFlowProps } from "./LaserFlow/LaserFlow.jsx";
import { cx, weakDevice } from "./ui";

// Fetched only when a shader actually runs, so phones never download it.
const LaserFlow = lazy(() => import("./LaserFlow/LaserFlow.jsx"));

/**
 * Shader beam or CSS fallback. The shader needs WebGL2 and a machine that can
 * afford a fragment shader every frame; weak hardware and data-saver get the
 * CSS pour, which is only gradients and transforms. Phones and tablets get the
 * shader too, rendered at a lower resolution (see `MOBILE_DPR`), and LaserFlow
 * steps the resolution down further if frames run slow. `null` until decided
 * on the client.
 */
export function useShader() {
  const [ok, setOk] = useState<boolean | null>(null);
  useEffect(() => {
    if (weakDevice()) {
      setOk(false);
      return;
    }
    try {
      setOk(!!document.createElement("canvas").getContext("webgl2"));
    } catch {
      setOk(false);
    }
  }, []);
  return ok;
}

/**
 * One look for every landing: the brand ember, kept in its own colour family
 * (hueSpread) so the light matches the rail, the glyphs and the accent.
 */
const EMBER: LaserFlowProps = {
  color: "#ff6a3d",
  hueSpread: 0.3,
  intensity: 0.8,
  beamWidth: 1.1,
  flare: 0.8,
  spread: 1.3,
  spill: 0.3,
  fog: 0.1,
  dust: 2,
  streaks: 2,
  dots: 1.75,
  pulse: 2,
  revealRadius: 220,
  revealOpacity: 0.6,
  dpr: 1.5,
};

// Small, dense touch screens: the glow is soft, so a 1x buffer looks the same
// and costs a third of the pixels a 3x phone would otherwise draw.
const MOBILE_DPR = 1;
const isMobile = () => window.matchMedia("(max-width: 767px), (pointer: coarse)").matches;

/**
 * The beam landing on a surface's top-left corner. The layer is placed by
 * `className`; it reaches from its own top down to the surface plus `below`
 * pixels (room for the light pouring over the corner), so the shader never
 * paints more of the page than it has to.
 */
export function LaserLanding({
  surfaceRef,
  className,
  below = 420,
  fadeTop = false,
  beamRef,
  ...props
}: {
  surfaceRef: RefObject<HTMLElement | null>;
  /** Where the beam falls, if not on the surface's top-left corner. */
  beamRef?: RefObject<HTMLElement | null>;
  className?: string;
  below?: number;
  fadeTop?: boolean;
} & LaserFlowProps) {
  const layerRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ height: 0, beam: 0.1 });

  useEffect(() => {
    const layer = layerRef.current;
    const surface = surfaceRef.current;
    if (!layer || !surface) return;
    const measure = () => {
      const own = layer.getBoundingClientRect();
      const rect = surface.getBoundingClientRect();
      const beamX = beamRef?.current ? beamRef.current.getBoundingClientRect().left : rect.left + 1;
      setBox({
        height: Math.round(rect.top - own.top + below),
        // Any point inside the left edge: LaserFlow clamps it to just past the corner radius.
        beam: (beamX - own.left) / Math.max(own.width, 1),
      });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(layer.parentElement!);
    ro.observe(surface);
    return () => ro.disconnect();
  }, [surfaceRef, beamRef, below]);

  return (
    <div
      ref={layerRef}
      aria-hidden
      className={cx(
        "pointer-events-none absolute",
        fadeTop
          ? "[mask-image:linear-gradient(to_bottom,transparent,#000_140px,#000_82%,transparent)]"
          : "[mask-image:linear-gradient(to_bottom,#000_82%,transparent)]",
        className,
      )}
      style={{ height: box.height || "100%" }}
    >
      {box.height > 0 && (
        <Suspense fallback={null}>
          <LaserFlow
            surfaceRef={surfaceRef}
            beamPosition={box.beam}
            {...EMBER}
            {...(isMobile() && { dpr: MOBILE_DPR })}
            {...props}
          />
        </Suspense>
      )}
    </div>
  );
}
