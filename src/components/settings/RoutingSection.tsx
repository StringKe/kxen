// 调度实况台：provider 槽位 / 角色绑定与降级链 / 试派发验证 / 最近派发历史。
// provider 下拉与默认模型来自后端 provider.list（registry），前端不维护硬编码清单。
import { createSignal, For, onMount, Show } from "solid-js";
import { Play } from "lucide-solid";
import { configGet, configSetRole, type RoleBindingView } from "../../lib/chat";
import {
  mrmStats,
  providerAccounts,
  providerList,
  testDispatch,
  type AccountInfo,
  type DispatchRecord,
  type MrmHealth,
  type ProviderInfo,
  type TestDispatchResult,
} from "../../lib/provider";
import { fmtCtx, modelsCatalog, type ProviderCatalog } from "../../lib/models";
import { errText } from "../err-text";
import { parseSlots, ROLE_LABELS, type Slot } from "./routing";
import { RoutingHistory, RoutingTelemetry } from "./RoutingTelemetry";

export default function RoutingSection() {
  const [roles, setRoles] = createSignal<Record<string, RoleBindingView>>({});
  const [slots, setSlots] = createSignal<Slot[]>([]);
  const [history, setHistory] = createSignal<DispatchRecord[]>([]);
  const [health, setHealth] = createSignal<MrmHealth[]>([]);
  const [accounts, setAccounts] = createSignal<AccountInfo[]>([]);
  const [providers, setProviders] = createSignal<ProviderInfo[]>([]);
  const [cat, setCat] = createSignal<ProviderCatalog[]>([]);
  const [testing, setTesting] = createSignal("");
  const [testResult, setTestResult] = createSignal<Record<string, TestDispatchResult>>({});
  const [saved, setSaved] = createSignal("");
  const [rolesLoaded, setRolesLoaded] = createSignal(false);
  const [rolesErr, setRolesErr] = createSignal("");
  const [sourceWarnings, setSourceWarnings] = createSignal("");
  const [actionErr, setActionErr] = createSignal("");
  const [savingRole, setSavingRole] = createSignal("");
  // model 被编辑过的角色：非法值（空/含空白）的行内提示只对编辑过的行显示，缺省空绑定不吵
  const [modelTouched, setModelTouched] = createSignal<Record<string, boolean>>({});
  let reloadSeq = 0;
  let telemetrySeq = 0;

  const reload = async () => {
    const seq = ++reloadSeq;
    const [cfg, stats, accs, catalog, list] = await Promise.allSettled([
      configGet(),
      mrmStats(),
      providerAccounts(),
      modelsCatalog(),
      providerList(),
    ]);
    if (seq !== reloadSeq) return;

    if (cfg.status === "fulfilled") {
      setRoles(cfg.value.roles ?? {});
      setRolesLoaded(true);
      setRolesErr("");
    } else {
      setRolesLoaded(false);
      setRolesErr(errText(cfg.reason));
    }
    if (stats.status === "fulfilled") {
      setSlots(parseSlots(stats.value.describe));
      setHistory(stats.value.history.slice(0, 10));
      setHealth(stats.value.health ?? []);
    }
    if (accs.status === "fulfilled") setAccounts(accs.value);
    if (catalog.status === "fulfilled") setCat(catalog.value);
    if (list.status === "fulfilled") setProviders(list.value as ProviderInfo[]);

    const warnings = [
      stats.status === "rejected" ? `MRM 状态：${errText(stats.reason)}` : "",
      accs.status === "rejected" ? `账号列表：${errText(accs.reason)}` : "",
      catalog.status === "rejected" ? `模型目录：${errText(catalog.reason)}` : "",
      list.status === "rejected" ? `Provider 列表：${errText(list.reason)}` : "",
    ].filter(Boolean);
    setSourceWarnings(warnings.join("；"));
  };
  onMount(() => void reload());

  const refreshRoles = async (): Promise<boolean> => {
    try {
      const cfg = await configGet();
      setRoles(cfg.roles ?? {});
      setRolesLoaded(true);
      setRolesErr("");
      return true;
    } catch (error) {
      setRolesLoaded(false);
      setRolesErr(errText(error));
      return false;
    }
  };

  const reloadTelemetry = async () => {
    const seq = ++telemetrySeq;
    try {
      const stats = await mrmStats();
      if (seq !== telemetrySeq) return;
      setSlots(parseSlots(stats.describe));
      setHistory(stats.history.slice(0, 10));
      setHealth(stats.health ?? []);
    } catch (error) {
      if (seq === telemetrySeq) setActionErr(`刷新 MRM 状态失败：${errText(error)}`);
    }
  };

  const flash = (msg: string) => {
    setSaved(msg);
    setTimeout(() => setSaved(""), 2000);
  };

  const accountOptions = (provider: string) => accounts().filter((a) => a.provider === provider);

  // update 永远以当前 binding 全量合并后下发（后端缺省会沿用旧值，见 settings.rs merge_binding）。
  // provider 变更时仅清 account（账号归属 provider，留旧账号会绑错）；fallback 是角色间降级关系，
  // 与 provider 无关，必须保留。想清除的字段显式传 ""（None/缺省在后端是「沿用旧值」语义）。
  const update = async (role: string, patch: Partial<RoleBindingView>) => {
    if (!rolesLoaded() || savingRole()) return;
    const cur = roles()[role] ?? {
      provider: "anthropic",
      model: "",
      account: null,
      fallback: null,
    };
    const next = { ...cur, ...patch };
    if (patch.provider !== undefined && patch.provider !== cur.provider) next.account = null;
    // 输入受控需要即时回显：先落地本地态；非法 model（空/含空白）不下发，行内提示接管
    const normalized = {
      provider: next.provider,
      model: next.model,
      account: next.account ?? null,
      fallback: next.fallback ?? null,
    };
    setRoles((prev) => ({ ...prev, [role]: normalized }));
    if (!next.model.trim() || /\s/.test(next.model)) return;
    setSavingRole(role);
    setActionErr("");
    try {
      await configSetRole(role, next.provider, next.model, next.fallback ?? "", next.account ?? "");
      flash(`${ROLE_LABELS[role] ?? role} 已保存并热生效`);
    } catch (error) {
      // RPC 可能在配置持久化后才失败，先读回权威配置；读回也失败时标记 UNKNOWN 并禁止继续写。
      if (!(await refreshRoles())) {
        setRoles((prev) => (prev[role] === normalized ? { ...prev, [role]: cur } : prev));
      }
      setActionErr(`${ROLE_LABELS[role] ?? role} 保存失败：${errText(error)}`);
    } finally {
      setSavingRole("");
    }
  };

  // a<->b 互指降级会循环空转：提示引导用户自拆环，不硬拦（配置是显式选择，拦截反而挡合法中间态）
  const cycleWith = (role: string) => {
    const f = roles()[role]?.fallback;
    return f && roles()[f]?.fallback === role ? f : null;
  };

  const tryDispatch = async (role: string) => {
    setTesting(role);
    try {
      const r = await testDispatch(role);
      setTestResult((prev) => ({ ...prev, [role]: r }));
      await reloadTelemetry();
    } catch (error) {
      setActionErr(`${ROLE_LABELS[role] ?? role} 试派发失败：${errText(error)}`);
    } finally {
      setTesting("");
    }
  };

  const defaultModelOf = (provider: string) =>
    providers().find((p) => p.key === provider)?.default_model ?? "";

  return (
    <>
      <div class="text-xs text-[var(--text-faint)]">
        调度实况（MRM 全局路由；槽位为空自动降级到 fallback 角色）
      </div>
      <Show when={saved()}>
        <div class="text-xs text-[var(--ok)]">{saved()}</div>
      </Show>
      <Show when={rolesErr()}>
        <div class="text-xs text-[var(--err)]">
          路由配置读取失败，当前值为 UNKNOWN：{rolesErr()}
          <button class="ml-2 hover:underline" onClick={() => void reload()}>
            重试
          </button>
        </div>
      </Show>
      <Show when={sourceWarnings()}>
        <div class="text-xs text-[var(--warn)]">部分状态加载失败：{sourceWarnings()}</div>
      </Show>
      <Show when={actionErr()}>
        <div class="text-xs text-[var(--err)]">{actionErr()}</div>
      </Show>

      <RoutingTelemetry slots={slots()} health={health()} />

      <div class="list-card">
        <For each={Object.keys(ROLE_LABELS)}>
          {(role) => {
            const binding = () => roles()[role] ?? { provider: "anthropic", model: "" };
            const result = () => testResult()[role];
            return (
              <div class="px-4 py-3">
                <div class="flex items-center gap-3">
                  <div class="w-20 shrink-0">
                    <div class="text-sm">{ROLE_LABELS[role]}</div>
                    <div class="text-2xs text-[var(--text-faint)]">{role}</div>
                  </div>
                  <select
                    class="form-select"
                    value={binding().provider}
                    disabled={!rolesLoaded() || Boolean(savingRole())}
                    onChange={(e) => {
                      const provider = e.currentTarget.value;
                      void update(role, {
                        provider,
                        model: defaultModelOf(provider) || binding().model,
                      });
                    }}
                  >
                    <For each={providers()}>
                      {(p) => <option value={p.key}>{p.display}</option>}
                    </For>
                  </select>
                  <select
                    class="form-select"
                    title="账号：未固定时按默认账号和命名账号选择；凭证不可用或账号 RPM 受限时尝试其他账号"
                    value={binding().account ?? ""}
                    disabled={!rolesLoaded() || Boolean(savingRole())}
                    onChange={(e) => void update(role, { account: e.currentTarget.value || null })}
                  >
                    <option value="">账号轮转</option>
                    <For each={accountOptions(binding().provider)}>
                      {(a) => <option value={a.account}>{a.account}</option>}
                    </For>
                  </select>
                  <input
                    list={`models-${role}`}
                    class="flex-1 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs font-mono"
                    value={binding().model}
                    placeholder="model id（可下拉搜索）"
                    disabled={!rolesLoaded() || Boolean(savingRole())}
                    onChange={(e) => {
                      setModelTouched((p) => ({ ...p, [role]: true }));
                      void update(role, { model: e.currentTarget.value });
                    }}
                  />
                  <datalist id={`models-${role}`}>
                    <For each={cat().find((p) => p.provider === binding().provider)?.models ?? []}>
                      {(m) => (
                        <option value={m.id}>{`${m.name} · ctx ${fmtCtx(m.context)}`}</option>
                      )}
                    </For>
                  </datalist>
                  <select
                    class="form-select"
                    title="降级目标角色：本角色槽位满时降级到该角色"
                    value={binding().fallback ?? ""}
                    disabled={!rolesLoaded() || Boolean(savingRole())}
                    onChange={(e) => void update(role, { fallback: e.currentTarget.value || null })}
                  >
                    <option value="">无降级</option>
                    <For each={Object.keys(ROLE_LABELS).filter((r) => r !== role)}>
                      {(r) => <option value={r}>{ROLE_LABELS[r]}</option>}
                    </For>
                  </select>
                  <Show when={binding().fallback}>
                    <span class="text-2xs text-[var(--text-faint)]" title="降级目标角色">
                      {"->"} {binding().fallback}
                    </span>
                  </Show>
                  <Show
                    when={
                      modelTouched()[role] &&
                      (!binding().model.trim() || /\s/.test(binding().model))
                    }
                  >
                    <span class="text-2xs text-[var(--warn)]">model 为空或含空白，未保存</span>
                  </Show>
                  <Show when={cycleWith(role)}>
                    {(f) => (
                      <span
                        class="text-2xs text-[var(--warn)]"
                        title="两角色互指降级会循环空转，只保留一个方向"
                      >
                        与{ROLE_LABELS[f()] ?? f()}互指降级
                      </span>
                    )}
                  </Show>
                  <button
                    class="pressable flex items-center gap-1 px-2 py-1 rounded text-2xs border border-[var(--border)]"
                    disabled={!rolesLoaded() || Boolean(testing()) || Boolean(savingRole())}
                    onClick={() => void tryDispatch(role)}
                    title="真实派发一个 PONG 子代理验证路由"
                  >
                    <Play size={10} />
                    {testing() === role ? "派发中" : "试派发"}
                  </button>
                </div>
                <Show when={result()}>
                  {(r) => (
                    <div class="mt-1.5 text-2xs text-[var(--text-faint)]">
                      实测路由：{r().provider}/{r().model}
                      <Show when={r().account}>（账号 {r().account}）</Show>
                      <Show when={r().degraded_from}>（降级自 {r().degraded_from}）</Show> · 应答：
                      {r().answer}
                    </div>
                  )}
                </Show>
              </div>
            );
          }}
        </For>
      </div>

      <RoutingHistory history={history()} />
    </>
  );
}
