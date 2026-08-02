// ProvidersSection 行展示纯逻辑（350 行门禁抽离）：行类型、标签与状态徽标。
import type { AccountInfo, ModelsResult, ProviderInfo, VerifyOutcome } from "../../lib/provider";

export interface Row extends AccountInfo {
  verify?: VerifyOutcome;
  verifying: boolean;
  usedBy: string[];
  modelsResult?: ModelsResult;
}

/** 行标签：display + 区域后缀（如「Kimi 中国版」）；存量无 region 账号只有 display。 */
export function labelOf(spec: ProviderInfo | undefined, r: Row): string {
  const region = r.region ? spec?.regions.find((x) => x.key === r.region) : undefined;
  return `${spec?.display ?? r.provider}${region ? ` ${region.display}` : ""}`;
}

/** 状态徽标：实测中 > 实测结果 > 过期 > 凭证在位未实测。 */
export function badge(r: Row): { text: string; cls: string } {
  if (r.verifying) return { text: "实测中…", cls: "text-[var(--text-faint)]" };
  if (r.verify) {
    if (r.verify.ok)
      return {
        text: `实测正常 ${(r.verify.latency_ms / 1000).toFixed(1)}s`,
        cls: "text-[var(--ok)]",
      };
    return { text: "实测失败", cls: "text-[var(--err)]" };
  }
  if (r.expired) return { text: "已过期", cls: "text-[var(--err)]" };
  return { text: "凭证在位（未实测）", cls: "text-[var(--warn)]" };
}
