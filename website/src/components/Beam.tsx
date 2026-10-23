import { motion, useScroll, useTransform } from "framer-motion";
import { type ReactNode, useEffect, useRef, useState } from "react";

/**
 * The page rail: the beam that left the hero keeps falling down the left edge
 * of the content, through every section, to the footer's horizon. The lit
 * part follows the reader (its head sits ~70% down the viewport), so the
 * light pours down as you scroll. Only transforms change per frame.
 */
export function BeamTrack({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ top: 0, h: 1, maxY: 1 });
  const { scrollY } = useScroll();

  useEffect(() => {
    const el = ref.current!;
    const measure = () =>
      setBox({
        top: el.getBoundingClientRect().top + window.scrollY,
        h: el.offsetHeight,
        maxY: document.documentElement.scrollHeight - window.innerHeight,
      });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    window.addEventListener("resize", measure);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, []);

  // Over the last ~half screen of scrolling the head catches up with the
  // horizon, so it always lands there however tall the viewport is.
  const lit = useTransform(scrollY, (y) => {
    const vh = window.innerHeight;
    const catchUp = Math.max(0, 1 - (box.maxY - y) / (vh * 0.6)) * vh * 0.45;
    return Math.min(box.h, Math.max(0, y + vh * 0.72 - box.top + catchUp));
  });
  const scaleY = useTransform(lit, (v) => v / box.h);

  return (
    <div ref={ref} className="relative">
      <div aria-hidden className="pointer-events-none absolute inset-y-0 left-1/2 w-full max-w-[1200px] -translate-x-1/2 px-4 sm:px-6 lg:px-8">
        <div className="relative h-full" style={{ ["--h" as string]: `${box.h}px` }}>
          <div className="rail" />
          <span className="drip" style={{ ["--dur" as string]: "11s" }} />
          <span className="drip" style={{ ["--dur" as string]: "15s", ["--delay" as string]: "-6s" }} />
          <span className="drip" style={{ ["--dur" as string]: "9s", ["--delay" as string]: "-3s" }} />
          <motion.div className="rail-lit" style={{ scaleY }} />
          <motion.div className="rail-head" style={{ y: lit }} />
        </div>
      </div>
      {children}
    </div>
  );
}
