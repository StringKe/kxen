// 订阅状态台（多账号）：默认账号官方导入 + 命名账号手动添加；逐账号实测与修复指引。
// provider 标签与区域来自后端 provider.list（registry），前端不维护硬编码清单。
import { createSignal, For, onMount, Show } from "solid-js";
import { Plus, RefreshCw, Trash2, Wrench } from "lucide-solid";
import { configGet } from "../../lib/chat";
import {
  providerAccounts,
  providerList,
  providerModels,
  providerReprobe,
  providerVerify,
  removeAccount,
  removeCustomProvider,
  setAccountRegion,
  type ProviderInfo,
  type ReprobeIssue,
} from "../../lib/provider";
import { GUIDES } from "../../lib/provider-guides";
import { flashErr, flashOk } from "../../lib/flash";
import { formatError } from "../../lib/error-text";
import AddAccountPanel from "./AddAccountPanel";
import ProviderCompatibility from "./ProviderCompatibility";
import { badge, labelOf, type Row } from "./providers-row";
import { errText } from "../err-text";

// 实测结果与拉模型条数是时点探测：切分区重挂载后需用户重新点按获取，
// 不缓存陈旧探测结果上屏（缓存会误导，探测本身一键可重发）。

export default function ProvidersSection() {
  const [rows, setRows] = createSignal<Row[]>([]);
  const [providers, setProviders] = createSignal<ProviderInfo[]>([]);
  const [reprobing, setReprobing] = createSignal(false);
  const [issues, setIssues] = createSignal<ReprobeIssue[]>([]);
  const [adding, setAdding] = createSignal(false);
  const [guideFor, setGuideFor] = createSignal("");
  // 待确认删除的行 id（所有删除先出行内确认条，占用中的账号条内列明受影响角色）
  const [confirmDel, setConfirmDel] = createSignal("");

  const specOf = (key: string) => providers().find((p) => p.key === key);
  const regionsOf = (r: Row) => specOf(r.provider)?.regions ?? [];

  const load = async () => {
    const [accounts, cfg, list] = await Promise.all([
      providerAccounts().catch(() => []),
      configGet().catch(() => null),
      providerList().catch(() => [] as ProviderInfo[]),
    ]);
    setProviders(list);
    const usedBy = new Map<string, string[]>();
    for (const [role, b] of Object.entries(cfg?.roles ?? {})) {
      const key = b.account ? `${b.provider}:${b.account}` : b.provider;
      usedBy.set(key, [...(usedBy.get(key) ?? []), role]);
    }
    setRows(accounts.map((a) => ({ ...a, verifying: false, usedBy: usedBy.get(a.id) ?? [] })));
  };

  const verifyOne = async (row: Row) => {
    setRows((prev) => prev.map((r) => (r.id === row.id ? { ...r, verifying: true } : r)));
    const account = row.account === "default" ? undefined : row.account;
    const v = await providerVerify(row.provider, account).catch((e) => ({
      ok: false,
      latency_ms: 0,
      detail: String(e),
    }));
    setRows((prev) =>
      prev.map((r) => (r.id === row.id ? { ...r, verifying: false, verify: v } : r)),
    );
  };

  /** 手动拉取模型清单（端点 /models）：成功显示条数，失败就地显错（不伪装空清单）。 */
  const fetchModels = async (row: Row) => {
    const account = row.account === "default" ? undefined : row.account;
    const r = await providerModels(row.provider, account).catch((e: unknown) => ({
      models: [] as string[],
      source: "error",
      detail: errText(e),
    }));
    setRows((prev) => prev.map((x) => (x.id === row.id ? { ...x, modelsResult: r } : x)));
  };

  /** 改区域：region 空串 = 清掉回落缺省区域（registry 首条）。 */
  const changeRegion = async (row: Row, region: string) => {
    try {
      await setAccountRegion(row.provider, row.account, region || undefined);
      flashOk(`已更新 ${row.id} 区域`);
      await load();
    } catch (e) {
      flashErr(`改区域失败：${errText(e)}`);
    }
  };

  const verifyAll = () => rows().forEach((r) => void verifyOne(r));

  // 打开页面零自动请求（同类型产品共识：探测只在首次导入 + 用户主动点）
  onMount(() => void load());

  const reprobe = async () => {
    setReprobing(true);
    setIssues([]);
    try {
      const r = await providerReprobe();
      setIssues(r.issues ?? []); // 未导入条目常驻，下一次重新导入才清
      flashOk(`已重新导入（${r.outcomes.join("，")}）`);
      await load();
      verifyAll(); // 重新导入 = 用户主动动作，导入后逐个验证一次
    } catch (e) {
      flashErr(`重新导入失败：${errText(e)}`);
    } finally {
      setReprobing(false);
    }
  };

  // 删除统一走行内确认条（对齐会话删除/worktree 的二次确认模式）：被角色占用的账号
  // 在条内列明受影响角色，未占用的也只少一次误触点击的代价
  const requestRemove = (row: Row) => setConfirmDel(row.id);

  const doRemove = async (row: Row) => {
    setConfirmDel("");
    try {
      if (row.custom) await removeCustomProvider(row.provider.slice("custom:".length));
      else await removeAccount(row.provider, row.account);
      flashOk(`已删除 ${row.id}`);
      await load();
    } catch (e) {
      flashErr(`删除失败：${errText(e)}`);
    }
  };

  return (
    <>
      <div class="flex items-center justify-between">
        <div class="text-xs text-[var(--text-faint)]">
          订阅实况（多账号 quota 池化；默认账号官方导入，命名账号手动添加）
        </div>
        <div class="flex items-center gap-1.5">
          <button
            class="pressable flex items-center gap-1 px-2 py-1 rounded text-xs text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60"
            onClick={() => setAdding(!adding())}
          >
            <Plus size={12} />
            添加账号
          </button>
          <button
            class="pressable flex items-center gap-1 px-2 py-1 rounded text-xs text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60"
            disabled={reprobing()}
            onClick={() => void reprobe()}
          >
            <RefreshCw size={12} class={reprobing() ? "animate-spin" : ""} />
            重新导入
          </button>
        </div>
      </div>

      <ProviderCompatibility providers={providers()} />

      <Show when={issues().length > 0}>
        <div class="rounded border border-[var(--warn)]/50 bg-[var(--warn)]/5 px-3 py-2 text-xs space-y-0.5">
          <div class="text-[var(--warn)]">以下订阅未导入（官方源无凭证）：</div>
          <For each={issues()}>
            {(i) => (
              <div
                class="text-[var(--text-dim)]"
                title={i.hint ? `探测位置：${i.hint}` : undefined}
              >
                {i.text}
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={adding()}>
        <AddAccountPanel
          onDone={(msg) => {
            setAdding(false);
            flashOk(msg);
            void load();
          }}
        />
      </Show>

      <div class="list-card">
        <For each={rows()}>
          {(r) => {
            const b = () => badge(r);
            return (
              <div class="px-4 py-3">
                <div class="flex items-center justify-between">
                  <div>
                    <div class="text-sm font-medium">
                      {labelOf(specOf(r.provider), r)}
                      <Show when={r.account !== "default"}>
                        <span class="text-[var(--text-faint)]"> · {r.account}</span>
                      </Show>
                    </div>
                    <div class="text-xs text-[var(--text-faint)]">
                      {r.id}
                      <Show when={r.usedBy.length > 0}> · 被 {r.usedBy.join("/")} 使用</Show>
                      <Show when={!r.custom && regionsOf(r).length > 1}>
                        <span>
                          {" · 区域："}
                          <select
                            class="bg-transparent border border-[var(--border)] rounded px-1 py-0 text-2xs text-[var(--text-dim)]"
                            title="运营区域（账号凭证只对该区域端点有效）"
                            value={r.region ?? ""}
                            onChange={(e) => void changeRegion(r, e.currentTarget.value)}
                          >
                            <option value="">{`缺省（${regionsOf(r)[0]?.display ?? ""}）`}</option>
                            <For each={regionsOf(r)}>
                              {(x) => <option value={x.key}>{x.display}</option>}
                            </For>
                          </select>
                        </span>
                      </Show>
                    </div>
                  </div>
                  <div class="flex items-center gap-2">
                    <div class={`text-sm font-medium ${b().cls}`}>{b().text}</div>
                    <button
                      class="pressable px-2 py-1 rounded text-2xs border border-[var(--border)]"
                      onClick={() => void verifyOne(r)}
                    >
                      实测
                    </button>
                    <button
                      class="pressable px-2 py-1 rounded text-2xs border border-[var(--border)]"
                      title="从端点拉取模型清单"
                      onClick={() => void fetchModels(r)}
                    >
                      拉模型
                    </button>
                    <Show when={r.custom}>
                      <button
                        class="pressable px-1.5 py-1 rounded text-[var(--text-faint)] hover:text-[var(--err)]"
                        title="删除自定义提供商"
                        onClick={() => requestRemove(r)}
                      >
                        <Trash2 size={12} />
                      </button>
                    </Show>
                    <Show
                      when={
                        !r.custom &&
                        r.account === "default" &&
                        (GUIDES[r.provider]?.length ?? 0) > 0
                      }
                    >
                      <button
                        class="pressable px-2 py-1 rounded text-2xs border border-[var(--border)]"
                        title="修复指引"
                        onClick={() => setGuideFor(guideFor() === r.provider ? "" : r.provider)}
                      >
                        <Wrench size={11} />
                      </button>
                    </Show>
                    <Show when={!r.custom && r.account !== "default"}>
                      <button
                        class="pressable px-1.5 py-1 rounded text-[var(--text-faint)] hover:text-[var(--err)]"
                        title="删除账号"
                        onClick={() => requestRemove(r)}
                      >
                        <Trash2 size={12} />
                      </button>
                    </Show>
                  </div>
                </div>
                <Show when={r.verify && !r.verify.ok}>
                  <div class="mt-1.5 text-xs text-[var(--err)] break-all">
                    {formatError(r.verify?.detail ?? "")}
                  </div>
                </Show>
                <Show when={r.modelsResult}>
                  {(m) => (
                    <Show
                      when={m().source === "endpoint"}
                      fallback={
                        <div class="mt-1 text-2xs text-[var(--err)] break-all">
                          拉取模型失败：{formatError(m().detail)}
                        </div>
                      }
                    >
                      <div class="mt-1 text-2xs text-[var(--text-faint)]">
                        端点模型：{m().models.length} 个（已并入 composer 模型选择器）
                      </div>
                    </Show>
                  )}
                </Show>
                <Show when={confirmDel() === r.id}>
                  <div class="mt-2 rounded border border-[var(--warn)]/50 bg-[var(--warn)]/5 px-3 py-2 text-xs space-y-2">
                    <div class="text-[var(--warn)]">
                      {r.usedBy.length > 0
                        ? `该账号正被 ${r.usedBy.join(" / ")} 使用，删除后这些角色将失去可用凭证。`
                        : `确认删除 ${r.id}？删除后需重新添加才能恢复使用。`}
                    </div>
                    <div class="flex gap-2">
                      <button
                        class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--err)] text-[var(--err)]"
                        onClick={() => void doRemove(r)}
                      >
                        确认删除
                      </button>
                      <button
                        class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                        onClick={() => setConfirmDel("")}
                      >
                        取消
                      </button>
                    </div>
                  </div>
                </Show>
                <Show when={guideFor() === r.provider && r.account === "default"}>
                  <div class="mt-2 rounded border border-[var(--border)] bg-[var(--bg-overlay)]/50 px-3 py-2 text-xs space-y-1">
                    <For each={GUIDES[r.provider] ?? []}>{(g) => <div>{g}</div>}</For>
                  </div>
                </Show>
              </div>
            );
          }}
        </For>
      </div>
    </>
  );
}
