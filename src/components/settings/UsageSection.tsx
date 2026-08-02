// 用量与统计：持久 token 下界、趋势和当前进程内的路由解析记录。
// RPC 失败与真零严格区分：失败显错误态，不把加载失败渲成全零。
import { createSignal, For, onMount, Show } from "solid-js";
import { configGet } from "../../lib/chat";
import { client } from "../../lib/client";
import { errText } from "../err-text";
import { providerList, type ProviderInfo } from "../../lib/provider";
import { hasUnknownMetering, type UsageCompleteness } from "../../lib/usage";
import NumberField from "./NumberField";
import UsageCompletenessNotices from "./UsageCompletenessNotices";
import { createSeqGuard } from "../../lib/async-guard";

interface Overview extends UsageCompleteness {
  total_input: number;
  total_output: number;
  sessions: number;
  dispatches: number;
  by_model: Record<string, number>;
  today_input: number;
  today_output: number;
  daily: { date: string; input: number; output: number }[];
  metering_warning?: string | null;
}

export default function UsageSection() {
  const [data, setData] = createSignal<Overview | null>(null);
  const [loadErr, setLoadErr] = createSignal("");
  const [providers, setProviders] = createSignal<ProviderInfo[]>([]);
  const [provider, setProvider] = createSignal("");
  const [dailyBudget, setDailyBudget] = createSignal("");
  const [inputRate, setInputRate] = createSignal("");
  const [outputRate, setOutputRate] = createSignal("");
  const [costBudget, setCostBudget] = createSignal("");
  const [failureThreshold, setFailureThreshold] = createSignal("3");
  const [cooldownSeconds, setCooldownSeconds] = createSignal("60");
  const [saved, setSaved] = createSignal("");
  const [saveErr, setSaveErr] = createSignal("");
  const [limitsLoaded, setLimitsLoaded] = createSignal(false);
  const [limitsErr, setLimitsErr] = createSignal("");
  const [providerErr, setProviderErr] = createSignal("");
  const [limitsSaving, setLimitsSaving] = createSignal(false);
  let limitsSeq = 0;
  const overviewGuard = createSeqGuard();
  let cachedProviderLimits: NonNullable<
    Awaited<ReturnType<typeof configGet>>["limits"]
  >["providers"] = {};

  const load = async () => {
    const request = overviewGuard.next();
    const r = await client.rpc<Overview>("usage.overview").catch((e: unknown) => {
      if (overviewGuard.isCurrent(request)) setLoadErr(errText(e));
      return null;
    });
    if (r && overviewGuard.isCurrent(request)) {
      setData(r);
      setLoadErr("");
    }
  };
  const loadLimits = async () => {
    const seq = ++limitsSeq;
    const [cfgResult, listResult] = await Promise.allSettled([configGet(), providerList()]);
    if (seq !== limitsSeq) return;
    if (cfgResult.status === "rejected") {
      setLimitsLoaded(false);
      setLimitsErr(errText(cfgResult.reason));
      return;
    }

    const cfg = cfgResult.value;
    cachedProviderLimits = cfg.limits?.providers ?? {};
    setDailyBudget(cfg.limits?.daily_token_budget?.toString() ?? "");
    if (listResult.status === "fulfilled" && Array.isArray(listResult.value)) {
      setProviders(listResult.value);
      setProviderErr("");
    } else {
      setProviderErr(
        listResult.status === "rejected" ? errText(listResult.reason) : "Provider 列表响应格式无效",
      );
    }
    const providerKeys = [
      ...(listResult.status === "fulfilled" && Array.isArray(listResult.value)
        ? listResult.value.map((item) => item.key)
        : providers().map((item) => item.key)),
      ...Object.keys(cachedProviderLimits),
    ];
    const first = providerKeys.includes(provider()) ? provider() : (providerKeys[0] ?? "");
    setProvider(first);
    applyProviderLimit(first, cachedProviderLimits);
    setLimitsLoaded(true);
    setLimitsErr("");
  };
  onMount(() => {
    void load();
    void loadLimits();
  });

  const models = () => Object.entries(data()?.by_model ?? {}).sort((a, b) => b[1] - a[1]);
  const fmt = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
  const meteringUnknown = () => hasUnknownMetering(data());
  const knownTotal = (value: number) => `${meteringUnknown() ? "≥" : ""}${fmt(value)}`;
  const maxDaily = () => Math.max(1, ...(data()?.daily ?? []).map((day) => day.input + day.output));

  const applyProviderLimit = (
    id: string,
    limits: NonNullable<Awaited<ReturnType<typeof configGet>>["limits"]>["providers"],
  ) => {
    const item = limits?.[id];
    setInputRate(item?.input_usd_per_million?.toString() ?? "");
    setOutputRate(item?.output_usd_per_million?.toString() ?? "");
    setCostBudget(item?.daily_cost_budget_usd?.toString() ?? "");
    setFailureThreshold(item?.circuit_failure_threshold?.toString() ?? "3");
    setCooldownSeconds(item?.circuit_cooldown_seconds?.toString() ?? "60");
  };

  const nullableNumber = (value: string) => (value.trim() === "" ? null : Number(value));

  const saveLimits = async () => {
    if (!limitsLoaded() || limitsSaving()) return;
    // 先清上一轮反馈：失败时残留「已保存」绿字会与错误并存误导（后端对无 provider 的熔断字段返回明确错误，走 catch 上屏）
    setSaved("");
    setSaveErr("");
    setLimitsSaving(true);
    try {
      const params: Record<string, unknown> = {
        daily_token_budget: nullableNumber(dailyBudget()),
      };
      if (provider()) {
        Object.assign(params, {
          provider: provider(),
          input_usd_per_million: nullableNumber(inputRate()),
          output_usd_per_million: nullableNumber(outputRate()),
          daily_cost_budget_usd: nullableNumber(costBudget()),
          circuit_failure_threshold: nullableNumber(failureThreshold()),
          circuit_cooldown_seconds: nullableNumber(cooldownSeconds()),
        });
      }
      await client.rpc("config.set_limits", params);
      setSaveErr("");
      setSaved("已保存并热生效");
      setTimeout(() => setSaved(""), 2000);
    } catch (error) {
      setSaveErr(errText(error));
    } finally {
      setLimitsSaving(false);
    }
  };

  return (
    <div class="list-card">
      <Show when={loadErr()}>
        <div class="px-4 py-3 text-xs flex items-center gap-3">
          <span class="text-[var(--err)]">
            {data() ? "刷新用量统计失败，正在显示上次结果" : "加载用量统计失败"}：{loadErr()}
          </span>
          <button
            class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-[var(--text-dim)]"
            onClick={() => void load()}
          >
            重试
          </button>
        </div>
      </Show>
      <Show when={!data() && !loadErr()}>
        <div class="px-4 py-3 text-xs text-[var(--text-faint)]">加载中…</div>
      </Show>
      <Show when={data()}>
        <div class="grid grid-cols-4 gap-2 px-4 py-3">
          <div>
            <div class="text-2xs text-[var(--text-faint)]">输入 tokens</div>
            <div class="text-sm tabular-nums">{knownTotal(data()?.total_input ?? 0)}</div>
          </div>
          <div>
            <div class="text-2xs text-[var(--text-faint)]">输出 tokens</div>
            <div class="text-sm tabular-nums">{knownTotal(data()?.total_output ?? 0)}</div>
          </div>
          <div>
            <div class="text-2xs text-[var(--text-faint)]">会话</div>
            <div class="text-sm tabular-nums">{data()?.sessions ?? 0}</div>
          </div>
          <div>
            <div class="text-2xs text-[var(--text-faint)]">最近路由解析记录</div>
            <div class="text-sm tabular-nums">{data()?.dispatches ?? 0}</div>
          </div>
        </div>
        <UsageCompletenessNotices usage={data()} />
        <div class="px-4 py-3">
          <div class="text-2xs text-[var(--text-faint)] mb-2">最近路由解析的按模型分布</div>
          <Show
            when={models().length > 0}
            fallback={<div class="text-xs text-[var(--text-faint)]">暂无路由解析记录</div>}
          >
            <div class="space-y-1">
              <For each={models()}>
                {([name, count]) => (
                  <div class="flex items-center gap-2 text-xs">
                    <span class="flex-1 truncate font-mono text-[var(--text-dim)]">{name}</span>
                    <span class="tabular-nums text-[var(--text-faint)]">{count} 条</span>
                  </div>
                )}
              </For>
            </div>
          </Show>
          <div class="text-2xs text-[var(--text-faint)] mt-3">
            路由解析最多保留当前进程最近 50 条，重启清空，不等于 Provider 调用或账单。Session
            累计以当前进程 ledger 展示；仅存储完整时确认已同步到
            usage.json。下方趋势按本地日期持久化， 并包含主会话、Subagent 和 Team。
          </div>
        </div>
        <div class="px-4 py-3">
          <div class="text-2xs text-[var(--text-faint)] mb-2">
            最近 14 天 token 趋势，今日{" "}
            {fmt((data()?.today_input ?? 0) + (data()?.today_output ?? 0))}
          </div>
          <div class="space-y-1">
            <For
              each={data()?.daily ?? []}
              fallback={<div class="text-xs text-[var(--text-faint)]">暂无持久趋势</div>}
            >
              {(day) => (
                <div class="flex items-center gap-2 text-xs">
                  <span class="w-20 text-[var(--text-faint)]">{day.date.slice(5)}</span>
                  <span class="ctx-bar flex-1">
                    <span
                      class="ctx-bar-fill"
                      style={`width:${((day.input + day.output) / maxDaily()) * 100}%`}
                    />
                  </span>
                  <span class="w-16 text-right tabular-nums text-[var(--text-dim)]">
                    {fmt(day.input + day.output)}
                  </span>
                </div>
              )}
            </For>
          </div>
        </div>
      </Show>

      <div class="px-4 py-3 space-y-3">
        <div>
          <div class="text-xs">MRM 预算和熔断</div>
          <div class="text-2xs text-[var(--text-faint)]">
            金额只按你填写的实际 USD 单价计算；订阅或未知价格保持 UNKNOWN。
          </div>
        </div>
        <Show when={limitsErr()}>
          <div class="text-xs text-[var(--err)]">
            加载限制配置失败，当前值为 UNKNOWN：{limitsErr()}
            <button class="ml-2 hover:underline" onClick={() => void loadLimits()}>
              重试
            </button>
          </div>
        </Show>
        <Show when={providerErr()}>
          <div class="text-xs text-[var(--warn)]">
            Provider 列表加载失败：{providerErr()}。仍可编辑配置中已知的 Provider。
          </div>
        </Show>
        <label class="block text-2xs text-[var(--text-faint)]">
          全局每日已结算 token 阈值，留空表示不限
          <input
            type="number"
            min="0"
            class="mt-1 w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
            value={dailyBudget()}
            disabled={!limitsLoaded() || limitsSaving()}
            onInput={(event) => setDailyBudget(event.currentTarget.value)}
          />
        </label>
        <div class="grid grid-cols-3 gap-2">
          <label class="text-2xs text-[var(--text-faint)]">
            Provider
            <select
              class="mt-1 w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
              value={provider()}
              disabled={!limitsLoaded() || limitsSaving()}
              onChange={(event) => {
                const id = event.currentTarget.value;
                setProvider(id);
                applyProviderLimit(id, cachedProviderLimits);
              }}
            >
              <For each={providers()}>
                {(item) => <option value={item.key}>{item.display}</option>}
              </For>
            </select>
          </label>
          <NumberField
            label="输入 USD / 1M"
            value={inputRate()}
            set={setInputRate}
            disabled={!limitsLoaded() || limitsSaving()}
          />
          <NumberField
            label="输出 USD / 1M"
            value={outputRate()}
            set={setOutputRate}
            disabled={!limitsLoaded() || limitsSaving()}
          />
          <NumberField
            label="每日已结算 USD 阈值"
            value={costBudget()}
            set={setCostBudget}
            disabled={!limitsLoaded() || limitsSaving()}
          />
          <NumberField
            label="连续失败阈值，0 关闭"
            value={failureThreshold()}
            set={setFailureThreshold}
            disabled={!limitsLoaded() || limitsSaving()}
          />
          <NumberField
            label="熔断冷却秒数"
            value={cooldownSeconds()}
            set={setCooldownSeconds}
            disabled={!limitsLoaded() || limitsSaving()}
          />
        </div>
        <div class="flex items-center gap-2">
          <button
            class="pressable px-3 py-1 rounded border border-[var(--border)] text-xs"
            disabled={!limitsLoaded() || limitsSaving()}
            onClick={() => void saveLimits()}
          >
            {limitsSaving() ? "保存中" : "保存限制"}
          </button>
          <Show when={saved()}>
            <span class="text-xs text-[var(--ok)]">{saved()}</span>
          </Show>
          <Show when={saveErr()}>
            <span class="text-xs text-[var(--err)]">保存失败：{saveErr()}</span>
          </Show>
        </div>
      </div>
    </div>
  );
}
