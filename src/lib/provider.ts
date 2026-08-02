import { client } from "./client";
import type { UsageCompleteness } from "./usage";

export interface RegionInfo {
  key: string;
  display: string;
  base_url: string;
}

/** 后端 providers registry 的投影（provider.list RPC）：前端 provider 下拉的唯一数据源。 */
export interface ProviderInfo {
  key: string;
  display: string;
  protocol: "anthropic" | "openai_compat";
  auth: "api_key" | "oauth" | "local_free";
  regions: RegionInfo[];
  models_endpoint: boolean;
  default_model: string;
  doc_url: string;
}

export function providerList(): Promise<ProviderInfo[]> {
  return client.rpc("provider.list");
}

export interface VerifyOutcome {
  ok: boolean;
  latency_ms: number;
  detail: string;
}

export function providerVerify(
  provider: string,
  account?: string,
  probe?: {
    access: string;
    kind: "oauth" | "api";
    refresh?: string;
    expires?: number;
    region?: string | undefined;
  },
): Promise<VerifyOutcome> {
  // probe 存在 = 添加账号面板的「测试连接」：候选凭证只进后端内存克隆，不落 auth.json
  return client.rpc("provider.verify", {
    provider,
    ...(account ? { account } : {}),
    ...(probe
      ? {
          access: probe.access,
          kind: probe.kind,
          refresh: probe.refresh ?? "",
          expires: probe.expires ?? 0,
          ...(probe.region ? { region: probe.region } : {}),
        }
      : {}),
  });
}

export interface ModelsResult {
  models: string[];
  source: string;
  detail: string;
}

export function providerModels(provider: string, account?: string): Promise<ModelsResult> {
  return client.rpc("provider.models", account ? { provider, account } : { provider });
}

export interface AccountInfo {
  provider: string;
  account: string;
  id: string;
  expired: boolean;
  region?: string | null;
  custom?: boolean;
  base_url?: string;
  models?: string[];
  protocol?: string;
  capabilities?: string[];
}

export function providerAccounts(): Promise<AccountInfo[]> {
  return client.rpc("provider.accounts");
}

export function importAccount(
  provider: string,
  account: string,
  access: string,
  kind: "oauth" | "api" = "oauth",
  refresh = "",
  expires = 0,
  region?: string,
): Promise<void> {
  return client.rpc("provider.import_account", {
    provider,
    account,
    access,
    kind,
    refresh,
    expires,
    ...(region ? { region } : {}),
  });
}

export function addCustomProvider(
  name: string,
  baseUrl: string,
  apiKey: string,
  models: string[],
  protocol: "openai" | "anthropic",
  capabilities: string[],
): Promise<void> {
  return client.rpc("provider.add_custom", {
    name,
    base_url: baseUrl,
    api_key: apiKey,
    models,
    protocol,
    capabilities,
  });
}

export function removeCustomProvider(name: string): Promise<void> {
  return client.rpc("provider.remove_custom", { name });
}

export function removeAccount(provider: string, account: string): Promise<void> {
  return client.rpc("provider.remove_account", { provider, account });
}

/** 改账号区域（多区域厂商）；region 缺省 = 清掉回落 registry 首条区域。 */
export function setAccountRegion(
  provider: string,
  account: string,
  region?: string,
): Promise<void> {
  return client.rpc("provider.set_region", { provider, account, ...(region ? { region } : {}) });
}

export interface ReprobeIssue {
  text: string; // 中文短句（如「ChatGPT Plus/Pro (codex)：未找到官方凭证」）
  hint: string; // 探测的官方源路径（常驻条目悬停 title）
}

export interface ReprobeResult {
  report: {
    entries: Array<{ provider: string; display: string; status: string; detail: string }>;
    data_dir: string;
    config_dir: string;
  };
  outcomes: string[]; // 全量短句（后端已映射中文）
  issues: ReprobeIssue[]; // 需用户处理的条目（官方源无凭证），前端常驻展示
}

export function providerReprobe(): Promise<ReprobeResult> {
  return client.rpc("provider.reprobe");
}

export interface DispatchRecord {
  role: string;
  provider: string;
  model: string;
  degraded_from?: string | null;
  at: number;
}

export interface MrmStats {
  describe: string;
  history: DispatchRecord[];
  health: MrmHealth[];
}

export interface MrmHealth extends UsageCompleteness {
  provider: string;
  consecutive_failures: number;
  circuit_open: boolean;
  cooldown_remaining_seconds: number;
  today_input: number;
  today_output: number;
  estimated_cost_usd?: number | null;
  daily_cost_budget_usd?: number | null;
}

export function mrmStats(): Promise<MrmStats> {
  return client.rpc("mrm.stats");
}

export interface TestDispatchResult {
  role: string;
  provider: string;
  model: string;
  account?: string | null;
  degraded_from?: string | null;
  answer: string;
}

export function testDispatch(role: string): Promise<TestDispatchResult> {
  return client.rpc("agent.test_dispatch", { role });
}
