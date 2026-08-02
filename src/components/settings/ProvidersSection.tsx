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
export default function ProvidersSection() {
  const [rows, setRows] = createSignal<Row[]>([]);
  const [providers, setProviders] = createSignal<ProviderInfo[]>([]);
  const [reprobing, setReprobing] = createSignal(false);
  const [issues, setIssues] = createSignal<ReprobeIssue[]>([]);
  const [adding, setAdding] = createSignal(false);
  const [guideFor, setGuideFor] = createSignal("");
  const [confirmDel, setConfirmDel] = createSignal("");
  const [loadErr, setLoadErr] = createSignal("");
  const [configLoaded, setConfigLoaded] = createSignal(false);
  let loadSeq = 0;

  const specOf = (key: string) => providers().find((p) => p.key === key);
  const regionsOf = (r: Row) => specOf(r.provider)?.regions ?? [];

  const load = async () => {
    const seq = ++loadSeq;
    const [accounts, cfg, list] = await Promise.allSettled([
      providerAccounts(),
      configGet(),
      providerList(),
    ]);
    if (seq !== loadSeq) return;
    if (list.status === "fulfilled") setProviders(list.value);
    setConfigLoaded(cfg.status === "fulfilled");
    const usedBy = new Map<string, string[]>();
    for (const [role, b] of Object.entries(cfg.status === "fulfilled" ? cfg.value.roles : {})) {
      const key = b.account ? `${b.provider}:${b.account}` : b.provider;
      usedBy.set(key, [...(usedBy.get(key) ?? []), role]);
    }
    if (accounts.status === "fulfilled") {
      const old = new Map(rows().map((row) => [row.id, row]));
      setRows(
        accounts.value.map((account) => ({
          ...account,
          verifying: false,
          usedBy:
            cfg.status === "fulfilled"
              ? (usedBy.get(account.id) ?? [])
              : (old.get(account.id)?.usedBy ?? []),
        })),
      );
    }
    setLoadErr(
      [accounts, cfg, list]
        .map((result, index) =>
          result.status === "rejected"
            ? `${["账号", "角色占用关系", "Provider 列表"][index]}：${errText(result.reason)}`
            : "",
        )
        .filter(Boolean)
        .join("；"),
    );
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

  const fetchModels = async (row: Row) => {
    const account = row.account === "default" ? undefined : row.account;
    const r = await providerModels(row.provider, account).catch((e: unknown) => ({
      models: [] as string[],
      source: "error",
      detail: errText(e),
    }));
    setRows((prev) => prev.map((x) => (x.id === row.id ? { ...x, modelsResult: r } : x)));
  };

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

  onMount(() => void load());

  const reprobe = async () => {
    setReprobing(true);
    setIssues([]);
    try {
      const r = await providerReprobe();
      setIssues(r.issues ?? []);
      flashOk(`已重新导入（${r.outcomes.join("，")}）`);
      await load();
      verifyAll(); // 重新导入 = 用户主动动作，导入后逐个验证一次
    } catch (e) {
      flashErr(`重新导入失败：${errText(e)}`);
    } finally {
      setReprobing(false);
    }
  };

  const requestRemove = (row: Row) => {
    if (configLoaded()) setConfirmDel(row.id);
    else flashErr("角色占用关系 UNKNOWN，当前不能安全删除账号");
  };

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
      <Show when={loadErr()}>
        <div class="text-xs text-[var(--err)]">
          Provider 数据加载失败，已保留上次结果：{loadErr()}
          <button class="ml-2 hover:underline" onClick={() => void load()}>
            重试
          </button>
        </div>
      </Show>

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
