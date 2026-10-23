// Small models that run well on old laptops (CPU only, 4–16 GB of RAM).
// Sizes are Ollama's default (Q4_K_M) downloads.

const MB = 1024 ** 2;
const GB = 1024 ** 3;

export interface CatalogModel {
  name: string;
  label: string;
  size: number;
  /** What it is good at, in a few words. */
  blurb: string;
  tags: ("chat" | "code" | "reasoning" | "embeddings" | "vision")[];
}

export const CATALOG: CatalogModel[] = [
  { name: "gemma3:1b", label: "Gemma 3 1B", size: 815 * MB, blurb: "Tiny and quick. Summaries, rewriting, simple chat.", tags: ["chat"] },
  { name: "llama3.2:1b", label: "Llama 3.2 1B", size: 1.3 * GB, blurb: "Fast assistant for short tasks.", tags: ["chat"] },
  { name: "qwen2.5-coder:1.5b", label: "Qwen 2.5 Coder 1.5B", size: 986 * MB, blurb: "Code completion and small edits.", tags: ["code"] },
  { name: "deepseek-r1:1.5b", label: "DeepSeek R1 1.5B", size: 1.1 * GB, blurb: "Thinks step by step before answering.", tags: ["reasoning"] },
  { name: "llama3.2:3b", label: "Llama 3.2 3B", size: 2.0 * GB, blurb: "Best all-rounder for an 8 GB machine.", tags: ["chat"] },
  { name: "qwen2.5:3b", label: "Qwen 2.5 3B", size: 1.9 * GB, blurb: "Strong at structured output and many languages.", tags: ["chat"] },
  { name: "qwen2.5-coder:3b", label: "Qwen 2.5 Coder 3B", size: 1.9 * GB, blurb: "Better code help, still light.", tags: ["code"] },
  { name: "phi4-mini", label: "Phi-4 mini 3.8B", size: 2.5 * GB, blurb: "Good reasoning and math for its size.", tags: ["chat", "reasoning"] },
  { name: "gemma3:4b", label: "Gemma 3 4B", size: 3.3 * GB, blurb: "Understands images as well as text.", tags: ["chat", "vision"] },
  { name: "qwen2.5:7b", label: "Qwen 2.5 7B", size: 4.7 * GB, blurb: "Noticeably smarter; wants 8 GB free.", tags: ["chat"] },
  { name: "llama3.1:8b", label: "Llama 3.1 8B", size: 4.9 * GB, blurb: "The classic 8B. Slow on old CPUs.", tags: ["chat"] },
  { name: "nomic-embed-text", label: "Nomic Embed", size: 274 * MB, blurb: "Embeddings for search and RAG in your apps.", tags: ["embeddings"] },
];

/** Memory a model needs while loaded: weights plus context and runtime. */
export function memoryNeeded(sizeBytes: number): number {
  return sizeBytes * 1.2 + 600 * MB;
}

export type Fit = "ok" | "tight" | "too-big";

/**
 * Will it run? `ok` when it fits in memory that is free right now, `tight`
 * when it only fits if other apps give some back, `too-big` otherwise.
 */
export function fit(sizeBytes: number, availableBytes: number, totalBytes: number): Fit {
  const need = memoryNeeded(sizeBytes);
  if (!totalBytes) return "ok";
  if (need <= availableBytes * 0.9) return "ok";
  if (need <= totalBytes * 0.7) return "tight";
  return "too-big";
}

/** Final line of an Ollama chat stream -> tokens per second. */
export function tokensPerSecond(final: { eval_count?: number; eval_duration?: number }): number | null {
  if (!final.eval_count || !final.eval_duration) return null;
  return final.eval_count / (final.eval_duration / 1e9);
}
