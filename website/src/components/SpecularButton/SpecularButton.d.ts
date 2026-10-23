import type { ButtonHTMLAttributes, JSX, ReactNode } from "react";

export interface SpecularButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  children?: ReactNode;
  size?: "sm" | "md" | "lg";
  radius?: number;
  tint?: string;
  tintOpacity?: number;
  blur?: number;
  textColor?: string;
  lineColor?: string;
  baseColor?: string;
  intensity?: number;
  idleIntensity?: number;
  shineSize?: number;
  shineFade?: number;
  thickness?: number;
  glow?: number;
  followMouse?: boolean;
  proximity?: number;
  autoAnimate?: boolean;
  speed?: number;
}

export default function SpecularButton(props: SpecularButtonProps): JSX.Element;
