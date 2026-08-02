// 模型目录前端快照：models.catalog RPC + 显示助手（picker/路由/状态栏共用一份缓存）。
import { client } from "./client";

export interface ModelInfo {
  id: string;
  name: string;
  family: string;
  reasoning: boolean;
  tool_call: boolean;
  attachment: boolean;
  modalities_in: string[];
  context: number;
  output: number;
}

export interface ProviderCatalog {
  provider: string;
  provider_name: string;
  models: ModelInfo[];
  fetched_at: number;
  source: string;
}

let cache: ProviderCatalog[] | null = null;

export async function modelsCatalog(force = false): Promise<ProviderCatalog[]> {
  if (cache && !force) return cache;
  cache = await client.rpc<ProviderCatalog[]>("models.catalog").catch(() => []);
  return cache;
}

export function modelOf(
  cat: ProviderCatalog[],
  provider: string,
  id: string,
): ModelInfo | undefined {
  return cat.find((p) => p.provider === provider)?.models.find((m) => m.id === id);
}

export function displayName(cat: ProviderCatalog[], provider: string, id: string): string {
  return modelOf(cat, provider, id)?.name ?? id;
}

/** ctx 窗格式化：1000000 -> 1M，262144 -> 256k。 */
export function fmtCtx(n: number): string {
  if (!n) return "";
  if (n >= 1_000_000) {
    const v = n / 1_000_000;
    return `${Number.isInteger(v) ? v : v.toFixed(1)}M`;
  }
  return `${Math.round(n / 1024)}k`;
}
