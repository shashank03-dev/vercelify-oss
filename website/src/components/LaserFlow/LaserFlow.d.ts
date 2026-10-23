import type { HTMLAttributes, JSX, RefObject } from "react";

export interface LaserFlowProps extends Omit<HTMLAttributes<HTMLDivElement>, "color"> {
  surfaceRef?: RefObject<HTMLElement | null>;
  beamPosition?: number;
  surfaceLevel?: number;
  color?: string;
  intensity?: number;
  beamWidth?: number;
  flare?: number;
  spread?: number;
  spill?: number;
  fog?: number;
  dust?: number;
  streaks?: number;
  dots?: number;
  pulse?: number;
  speed?: number;
  seed?: number;
  /** 0–1: how far the derived tints may drift in hue from `color`. */
  hueSpread?: number;
  mouseInteraction?: boolean;
  revealImage?: string;
  revealRadius?: number;
  revealOpacity?: number;
  intro?: boolean;
  theme?: "dark" | "light";
  paused?: boolean;
  dpr?: number;
}

export function LaserFlow(props: LaserFlowProps): JSX.Element;
export default LaserFlow;
