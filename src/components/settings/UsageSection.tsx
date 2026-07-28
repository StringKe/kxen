// 用量与统计：usage.overview 真实数据（tokens 汇总 + 模型派发分布）。
// RPC 失败与真零严格区分：失败显错误态，不把加载失败渲成全零。
import { createSignal, For, onMount, Show } from "solid-js";
import { configGet } from "../../lib/chat";
import { client } from "../../lib/client";
import { formatError } from "../../lib/error-text";
import { providerList, type ProviderInfo } from "../../lib/provider";

interface Overview {
  total_input: number;
  total_output: number;
  sessions: number;
  dispatches: number;
  by_model: Record<string, number>;
  today_input: number;
  today_output: number;
  daily: { date: string; input: number; output: number }[];
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

  const load = async () => {
    const r = await client.rpc<Overview>("usage.overview").catch((e: unknown) => {
      setLoadErr(formatError(e instanceof Error ? e.message : String(e)));
      return null;
    });
    if (r) {
      setData(r);
      setLoadErr("");
    }
  };
  const loadLimits = async () => {
    const [cfg, list] = await Promise.all([
      configGet().catch(() => null),
      providerList().catch(() => []),
    ]);
    const providerItems = Array.isArray(list) ? list : [];
    setProviders(providerItems);
    const first = providerItems[0]?.key ?? Object.keys(cfg?.limits?.providers ?? {})[0] ?? "";
    setProvider(first);
    setDailyBudget(cfg?.limits?.daily_token_budget?.toString() ?? "");
    applyProviderLimit(first, cfg?.limits?.providers ?? {});
  };
  onMount(() => {
    void load();
    void loadLimits();
  });

  const models = () => Object.entries(data()?.by_model ?? {}).sort((a, b) => b[1] - a[1]);
  const fmt = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
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
    try {
      await client.rpc("config.set_limits", {
        daily_token_budget: nullableNumber(dailyBudget()),
        provider: provider() || undefined,
        input_usd_per_million: nullableNumber(inputRate()),
        output_usd_per_million: nullableNumber(outputRate()),
        daily_cost_budget_usd: nullableNumber(costBudget()),
        circuit_failure_threshold: nullableNumber(failureThreshold()),
        circuit_cooldown_seconds: nullableNumber(cooldownSeconds()),
      });
      setSaveErr("");
      setSaved("已保存并热生效");
      setTimeout(() => setSaved(""), 2000);
    } catch (error) {
      setSaveErr(formatError(error instanceof Error ? error.message : String(error)));
    }
  };

  return (
    <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] divide-y divide-[var(--border)]">
      <Show when={loadErr()}>
        <div class="px-4 py-3 text-xs flex items-center gap-3">
          <span class="text-[var(--err)]">加载用量统计失败：{loadErr()}</span>
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
            <div class="text-sm tabular-nums">{fmt(data()?.total_input ?? 0)}</div>
          </div>
          <div>
            <div class="text-2xs text-[var(--text-faint)]">输出 tokens</div>
            <div class="text-sm tabular-nums">{fmt(data()?.total_output ?? 0)}</div>
          </div>
          <div>
            <div class="text-2xs text-[var(--text-faint)]">会话</div>
            <div class="text-sm tabular-nums">{data()?.sessions ?? 0}</div>
          </div>
          <div>
            <div class="text-2xs text-[var(--text-faint)]">派发次数</div>
            <div class="text-sm tabular-nums">{data()?.dispatches ?? 0}</div>
          </div>
        </div>
        <div class="px-4 py-3">
          <div class="text-2xs text-[var(--text-faint)] mb-2">按模型的派发分布</div>
          <Show
            when={models().length > 0}
            fallback={<div class="text-xs text-[var(--text-faint)]">暂无派发记录</div>}
          >
            <div class="space-y-1">
              <For each={models()}>
                {([name, count]) => (
                  <div class="flex items-center gap-2 text-xs">
                    <span class="flex-1 truncate font-mono text-[var(--text-dim)]">{name}</span>
                    <span class="tabular-nums text-[var(--text-faint)]">{count} 次</span>
                  </div>
                )}
              </For>
            </div>
          </Show>
          <div class="text-2xs text-[var(--text-faint)] mt-3">
            活跃 Session 累计用于状态栏；下方趋势按本地日期持久化，并包含主会话、Subagent 和 Team。
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
        <label class="block text-2xs text-[var(--text-faint)]">
          全局每日 token 上限，留空表示不限
          <input
            type="number"
            min="0"
            class="mt-1 w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
            value={dailyBudget()}
            onInput={(event) => setDailyBudget(event.currentTarget.value)}
          />
        </label>
        <div class="grid grid-cols-3 gap-2">
          <label class="text-2xs text-[var(--text-faint)]">
            Provider
            <select
              class="mt-1 w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
              value={provider()}
              onChange={async (event) => {
                const id = event.currentTarget.value;
                setProvider(id);
                const cfg = await configGet().catch(() => null);
                applyProviderLimit(id, cfg?.limits?.providers ?? {});
              }}
            >
              <For each={providers()}>
                {(item) => <option value={item.key}>{item.display}</option>}
              </For>
            </select>
          </label>
          <NumberField label="输入 USD / 1M" value={inputRate()} set={setInputRate} />
          <NumberField label="输出 USD / 1M" value={outputRate()} set={setOutputRate} />
          <NumberField label="每日 USD 上限" value={costBudget()} set={setCostBudget} />
          <NumberField
            label="连续失败阈值，0 关闭"
            value={failureThreshold()}
            set={setFailureThreshold}
          />
          <NumberField label="熔断冷却秒数" value={cooldownSeconds()} set={setCooldownSeconds} />
        </div>
        <div class="flex items-center gap-2">
          <button
            class="pressable px-3 py-1 rounded border border-[var(--border)] text-xs"
            onClick={() => void saveLimits()}
          >
            保存限制
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

function NumberField(props: { label: string; value: string; set: (value: string) => void }) {
  return (
    <label class="text-2xs text-[var(--text-faint)]">
      {props.label}
      <input
        type="number"
        min="0"
        step="any"
        class="mt-1 w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
        value={props.value}
        onInput={(event) => props.set(event.currentTarget.value)}
      />
    </label>
  );
}
