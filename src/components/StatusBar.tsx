import { createEffect, createSignal, Show, onCleanup, onMount } from "solid-js";
import { GitBranch, Target, ListTodo } from "lucide-solid";
import NotificationCenter from "./NotificationCenter";
import { statusline, type StatuslineReport } from "../lib/chat";
import { displayName, fmtCtx, modelOf, modelsCatalog, type ProviderCatalog } from "../lib/models";
import { goalStatusMeta } from "../lib/board";
import { activeSessionId } from "../lib/state";
import {
  hasUnknownMetering,
  hasUnknownStorage,
  usageMeteringUnknownDetail,
  usageStorageUnknownDetail,
} from "../lib/usage";
import { createSeqGuard } from "../lib/async-guard";
import { formatError } from "../lib/error-text";

/** 底部状态栏：固定段 + config 开关，3s 轮询 + 会话切换即时刷新。 */
export default function StatusBar() {
  const [snapshot, setSnapshot] = createSignal<{ sid: string; report: StatuslineReport } | null>(
    null,
  );
  const [cat, setCat] = createSignal<ProviderCatalog[]>([]);
  const [loadErr, setLoadErr] = createSignal("");
  const guard = createSeqGuard();
  let timer: ReturnType<typeof setInterval> | undefined;
  let observedSid: string | undefined;
  const report = () => {
    const current = snapshot();
    return current?.sid === activeSessionId() ? current.report : null;
  };

  const reload = async () => {
    const sid = activeSessionId();
    const request = guard.next();
    try {
      const next = await statusline(sid);
      if (guard.isCurrent(request) && activeSessionId() === sid) {
        setSnapshot({ sid, report: next });
        setLoadErr("");
      }
    } catch (error) {
      if (guard.isCurrent(request) && activeSessionId() === sid) setLoadErr(formatError(error));
    }
  };

  onMount(() => {
    void modelsCatalog()
      .then(setCat)
      .catch(() => {});
    timer = setInterval(() => void reload(), 3000);
  });
  onCleanup(() => timer && clearInterval(timer));

  // 会话切换即时换 tokens/ctx/model（否则最长 3s 显示上一会话数据）；首跑兼代 onMount 首拉
  createEffect(() => {
    const sid = activeSessionId();
    if (observedSid !== sid) {
      observedSid = sid;
      setSnapshot(null);
    }
    setLoadErr("");
    void reload();
  });

  const has = (item: string) => report()?.items.includes(item) ?? false;
  // "provider/model-id" -> models.dev 显示名（查不到回退原串）
  const modelLabel = () => {
    const raw = report()?.model ?? "";
    const slash = raw.indexOf("/");
    if (slash <= 0) return raw;
    return displayName(cat(), raw.slice(0, slash), raw.slice(slash + 1));
  };
  const shortWorkdir = () => {
    const w = report()?.workdir ?? "";
    const home = "/Users/";
    const idx = w.indexOf(home);
    return idx === 0 ? `~/${w.slice(home.length).split("/").slice(1).join("/")}` : w;
  };
  // goal 中文徽标：与看板/Dock 共用 board.ts 状态映射，渲染中文徽标而非原始英文 status
  const goalMeta = () => goalStatusMeta(report()?.goal?.status ?? "");
  const goalToneCls = () =>
    ({ ok: "text-[var(--ok)]", warn: "text-[var(--warn)]", dim: "text-[var(--text-dim)]" })[
      goalMeta().tone
    ];
  const sessionMeteringUnknown = () => hasUnknownMetering(report()?.tokens);
  const sessionStorageUnknown = () => hasUnknownStorage(report()?.tokens);
  // ctx 窗取 catalog 实测值（models.dev），不写死固定窗口文案
  const ctxWindow = () => {
    const raw = report()?.model ?? "";
    const slash = raw.indexOf("/");
    if (slash <= 0) return "";
    const m = modelOf(cat(), raw.slice(0, slash), raw.slice(slash + 1));
    return m ? fmtCtx(m.context) : "";
  };

  return (
    <div class="h-7 shrink-0 flex items-center gap-3 px-3 border-t border-[var(--border)] bg-[var(--bg-raised)] text-xs text-[var(--text-dim)]">
      <Show when={has("workdir")}>
        <span class="truncate max-w-60" title={report()?.workdir}>
          {shortWorkdir()}
        </span>
      </Show>
      <Show when={has("git") && report()?.git_branch}>
        <span class="flex items-center gap-1">
          <GitBranch size={11} />
          {report()?.git_branch}
        </span>
      </Show>
      <Show when={has("goal") && report()?.goal}>
        <span class={`flex items-center gap-1 ${goalToneCls()}`} title={report()?.goal?.id}>
          <Target size={11} />
          {goalMeta().label}
        </span>
      </Show>
      <Show when={has("tasks") && (report()?.tasks_running ?? 0) > 0}>
        <span class="flex items-center gap-1">
          <ListTodo size={11} />
          {report()?.tasks_running} 运行中
        </span>
      </Show>
      <span class="ml-auto flex items-center gap-3 tabular-nums">
        <NotificationCenter />
        <Show when={loadErr()}>
          <span class="text-[var(--warn)]" title={`状态栏加载失败：${loadErr()}`}>
            状态 UNKNOWN
          </span>
        </Show>
        <Show when={has("tokens")}>
          <span
            title={
              sessionMeteringUnknown() || sessionStorageUnknown()
                ? `本会话 tokens（input/output）：${[
                    sessionMeteringUnknown() ? usageMeteringUnknownDetail(report()?.tokens) : "",
                    sessionStorageUnknown() ? usageStorageUnknownDetail(report()?.tokens) : "",
                  ]
                    .filter(Boolean)
                    .join("；")}`
                : "本会话 tokens（input/output）"
            }
          >
            {sessionMeteringUnknown() ? "≥" : ""}
            {(report()?.tokens.input ?? 0).toLocaleString("en-US")}/
            {(report()?.tokens.output ?? 0).toLocaleString("en-US")}
            <Show when={sessionMeteringUnknown()}>
              <span class="ml-1 text-[var(--warn)]">计量 UNKNOWN</span>
            </Show>
            <Show when={sessionStorageUnknown()}>
              <span class="ml-1 text-[var(--warn)]">存储 UNKNOWN</span>
            </Show>
          </span>
        </Show>
        <Show when={has("ctx")}>
          <span
            class="flex items-center gap-1.5"
            title={`上下文占用（最近 run input / ${ctxWindow() || "?"} 窗）`}
          >
            <span class="ctx-bar">
              <span
                class="ctx-bar-fill"
                classList={{
                  "ctx-warn": (report()?.ctx_pct ?? 0) > 70 && (report()?.ctx_pct ?? 0) <= 90,
                  "ctx-err": (report()?.ctx_pct ?? 0) > 90,
                }}
                style={`width:${report()?.ctx_pct ?? 0}%`}
              />
            </span>
            <span
              classList={{
                "text-[var(--warn)]":
                  (report()?.ctx_pct ?? 0) > 70 && (report()?.ctx_pct ?? 0) <= 90,
                "text-[var(--err)]": (report()?.ctx_pct ?? 0) > 90,
              }}
            >
              {report()?.ctx_pct}%
            </span>
          </span>
        </Show>
        <Show when={has("model")}>
          <span class="text-[var(--text-faint)]" title={report()?.model}>
            {modelLabel()}
          </span>
        </Show>
      </span>
    </div>
  );
}
