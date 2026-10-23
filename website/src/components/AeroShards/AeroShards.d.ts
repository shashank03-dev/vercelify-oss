import type { CSSProperties, JSX } from "react";

export interface AeroShardsProps {
  shardColor?: string;
  accentColor?: string;
  backgroundColor?: string;
  lightMode?: boolean;
  placement?: "full" | "left" | "right" | "center";
  material?: "glass" | "chrome";
  markers?: "plus" | "dots" | "none";
  count?: number;
  shardSize?: number;
  spread?: number;
  twist?: number;
  billow?: number;
  speed?: number;
  scale?: number;
  depthOfField?: number;
  glow?: number;
  grain?: number;
  vignette?: number;
  intro?: boolean;
  mouseInteraction?: boolean;
  mouseStrength?: number;
  parallax?: number;
  pulseOnClick?: boolean;
  holdToGather?: boolean;
  paused?: boolean;
  dpr?: number;
  onError?: (error: Error) => void;
  className?: string;
  style?: CSSProperties;
}

export default function AeroShards(props: AeroShardsProps): JSX.Element;
