// 添加账号面板的模块级表单状态：面板随设置分区切换卸载重建，挂模块级 signal 半填表单不丢
// （成功保存才重置；kind/provider/protocol/caps 保存后也保留，连续添加同类账号是常态）。
import { createSignal } from "solid-js";

export type AccountKind = "oauth" | "apikey" | "custom";

export const [kind, setKind] = createSignal<AccountKind>("oauth");
export const [provider, setProvider] = createSignal("anthropic");
export const [region, setRegion] = createSignal("");
export const [name, setName] = createSignal("");
export const [token, setToken] = createSignal("");
export const [baseUrl, setBaseUrl] = createSignal("");
export const [models, setModels] = createSignal("");
export const [protocol, setProtocol] = createSignal<"openai" | "anthropic">("openai");
export const [caps, setCaps] = createSignal<string[]>(["text"]);

export const resetAccountForm = () => {
  setName("");
  setToken("");
  setBaseUrl("");
  setModels("");
  setRegion("");
};

// 名字进凭证键（provider:名）与 custom_providers 表键：冒号撕裂账号键解析，空白不可读
export const ACCOUNT_NAME_BAD = /[:：\s]/;

/** OAuth JSON 粘贴 -> 拆出 access/refresh/expires；`{` 开头但 JSON 损坏是明确错误，不静默降级。 */
export function parseAccountToken(
  kind: AccountKind,
  raw: string,
): {
  access: string;
  refresh: string;
  expires: number;
  error?: string; // 解析失败：调用方必须中止，不得当裸 token 用
  warning?: string; // 可继续但需提示用户
} {
  const access = raw.trim();
  if (kind !== "oauth" || !access) return { access, refresh: "", expires: 0 };
  // 缺 refresh_token 的凭证过期后无法自动续期，只能再贴一次
  const noRefresh = "缺少 refresh_token，token 过期后需重新手动粘贴";
  if (!access.startsWith("{")) return { access, refresh: "", expires: 0, warning: noRefresh };
  try {
    const j = JSON.parse(access) as {
      access_token?: string;
      refresh_token?: string;
      expires_at?: number;
    };
    const refresh = j.refresh_token ?? "";
    return {
      access: j.access_token ?? access,
      refresh,
      expires: j.expires_at ?? 0,
      ...(refresh ? {} : { warning: noRefresh }),
    };
  } catch (e) {
    return {
      access: "",
      refresh: "",
      expires: 0,
      error: `JSON 解析失败：${e instanceof Error ? e.message : String(e)}`,
    };
  }
}
