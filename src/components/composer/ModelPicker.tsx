// ModelPicker：catalog 驱动（models.dev 快照）——显示名 + id + ctx + 能力徽章 + 搜索 + 方向键导航 + 角色分配。
import { createEffect, createSignal, For, onMount, Show } from "solid-js";
import { Check, ChevronDown, Search } from "lucide-solid";
import { configSetRole } from "../../lib/chat";
import { sessionFollowGlobalModel, sessionSetModel } from "../../lib/session-model";
import { activeSessionId, sessions } from "../../lib/state";
import { createExclusiveDisclosure, onClickOutside } from "../../lib/dismiss";
import { flashErr } from "../../lib/flash";
import { errText } from "../err-text";
import {
  fmtCtx,
  modelOf,
  modelsCatalog,
  type ModelInfo,
  type ProviderCatalog,
} from "../../lib/models";
import ModelStatusErrors from "./ModelStatusErrors";
import { createModelStatus } from "./model-status";

const ROLE_ASSIGN: Array<{ role: string; label: string }> = [
  { role: "chat", label: "设为主会话模型" },
  { role: "thinking", label: "设为思考模型" },
  { role: "planning", label: "设为规划模型" },
  { role: "execution", label: "设为执行模型" },
  { role: "review", label: "设为审查模型" },
  { role: "research", label: "设为调研模型" },
];

interface Row {
  provider: string;
  providerName: string;
  model: ModelInfo;
}

export default function ModelPicker() {
  const { cur, setCur, curErr, globalDef, globalErr, reloadCurrent, reloadGlobal } =
    createModelStatus();
  const [cat, setCat] = createSignal<ProviderCatalog[]>([]);
  const [catLoading, setCatLoading] = createSignal(true);
  const [catErr, setCatErr] = createSignal("");
  const { open, setOpen, toggle } = createExclusiveDisclosure();
  const [query, setQuery] = createSignal("");
  const [roleMsg, setRoleMsg] = createSignal("");
  const [modelSaving, setModelSaving] = createSignal(false);
  // 键盘导航选中位：-1 = 未导航（Enter 落首行）；与 filtered() 同步失效（query 变即复位）
  const [nav, setNav] = createSignal(-1);
  // 本地选择优先于 sessions 列表推导（set_model 不触发列表刷新，meta 是旧值）
  const [followOverride, setFollowOverride] = createSignal<boolean | null>(null);
  let root: HTMLDivElement | undefined;
  let searchInput: HTMLInputElement | undefined;
  let listEl: HTMLDivElement | undefined;
  onClickOutside(
    () => root,
    () => setOpen(false),
  );

  const reloadCatalog = async (force = false) => {
    setCatLoading(true);
    try {
      setCat(await modelsCatalog(force));
      setCatErr("");
    } catch (error) {
      setCatErr(errText(error));
    } finally {
      setCatLoading(false);
    }
  };
  onMount(() => {
    void reloadCatalog();
  });
  createEffect(() => {
    activeSessionId();
    setFollowOverride(null);
  });
  // 打开弹层自动聚焦搜索框（挂上即输入），同时复位键盘导航
  createEffect(() => {
    if (open()) {
      setNav(-1);
      searchInput?.focus();
    }
  });
  // 键盘导航选中项滚进可视区（长列表方向键走到屏外等于没导航）
  createEffect(() => {
    const n = nav();
    if (n >= 0)
      listEl?.querySelectorAll<HTMLElement>("[data-nav]")[n]?.scrollIntoView({ block: "nearest" });
  });

  const rows = (): Row[] =>
    cat().flatMap((p) =>
      p.models.map((model) => ({ provider: p.provider, providerName: p.provider_name, model })),
    );
  const filtered = () => {
    const q = query().toLowerCase();
    if (!q) return rows();
    return rows().filter((r) =>
      `${r.providerName} ${r.model.name} ${r.model.id}`.toLowerCase().includes(q),
    );
  };

  const curInfo = () => modelOf(cat(), cur().provider, cur().model);
  const curLabel = () =>
    curErr()
      ? "模型 UNKNOWN"
      : (curInfo()?.name ?? (cur().model ? `${cur().provider}/${cur().model}` : "模型"));
  const globalLabel = () =>
    globalErr()
      ? "UNKNOWN"
      : (modelOf(cat(), globalDef().provider, globalDef().model)?.name ??
        (globalDef().model || "未设置"));
  // 跟随态：本地选择优先；否则按 session meta 有无覆盖推导（草稿态无 meta = 跟随）
  const following = () =>
    followOverride() ?? !sessions().find((s) => s.id === activeSessionId())?.model;

  // 切模型只写当前 session 的 metadata（草稿态暂存，落库后回写）；全局默认在设置页改
  const pick = (r: Row) => {
    if (modelSaving()) return;
    const sid = activeSessionId();
    const prev = cur();
    const prevFollow = followOverride();
    // 乐观更新：写失败回滚显示，pill 不能亮着一个没生效的模型
    setCur({ provider: r.provider, model: r.model.id });
    setFollowOverride(false);
    setOpen(false);
    setModelSaving(true);
    void sessionSetModel(sid, r.provider, r.model.id)
      .catch((e: unknown) => {
        if (activeSessionId() !== sid) return;
        setCur(prev);
        setFollowOverride(prevFollow);
        flashErr(`切换模型失败：${errText(e)}`);
      })
      .finally(() => setModelSaving(false));
  };

  // 跟随全局默认：清除 session 覆盖（后端 provider/model 同缺 = 清除），生效模型回到全局默认
  const followGlobal = () => {
    if (modelSaving()) return;
    const sid = activeSessionId();
    const prevFollow = followOverride();
    setFollowOverride(true);
    setOpen(false);
    setModelSaving(true);
    void sessionFollowGlobalModel(sid)
      .then(async () => {
        if (activeSessionId() !== sid) return;
        const error = await reloadCurrent(sid, true);
        if (error && activeSessionId() === sid)
          flashErr(`已跟随全局默认，但读取生效模型失败：${error}`);
      })
      .catch((e: unknown) => {
        if (activeSessionId() !== sid) return;
        setFollowOverride(prevFollow); // 清除没写成：跟随态回滚，免得显示与后端脱节
        flashErr(`跟随全局默认失败：${errText(e)}`);
      })
      .finally(() => setModelSaving(false));
  };

  const assignRole = (role: string, label: string) => {
    if (!cur().model) return;
    configSetRole(role, cur().provider, cur().model)
      .then(() => {
        setRoleMsg(`${curLabel()} → ${label.replace("设为", "")} ✓`);
        setTimeout(() => setRoleMsg(""), 1800);
      })
      .catch((e: unknown) => flashErr(`分配角色失败：${errText(e)}`));
  };

  function onSearchKey(e: KeyboardEvent) {
    const list = filtered();
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (list.length === 0) return;
      const d = e.key === "ArrowDown" ? 1 : -1;
      setNav((n) => (n + d + list.length) % list.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const r = list[nav() < 0 ? 0 : nav()];
      if (r) pick(r);
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  }

  return (
    <div class="relative" ref={(el) => (root = el)}>
      <button
        class="pressable model-pill"
        disabled={modelSaving()}
        aria-expanded={open()}
        aria-haspopup="listbox"
        onClick={toggle}
      >
        <span class="text-2xs text-[var(--text-faint)]">{curInfo()?.family ?? cur().provider}</span>
        <span class="model-pill-name">{curLabel()}</span>
        <Show when={curInfo()?.context}>
          <span class="text-2xs text-[var(--text-faint)]">{fmtCtx(curInfo()!.context)}</span>
        </Show>
        <ChevronDown size={12} />
      </button>

      <Show when={open()}>
        <div
          role="listbox"
          class="composer-popup absolute bottom-full right-0 mb-1.5 w-80 max-w-[calc(100vw-16px)] rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] overflow-hidden z-20"
        >
          <div class="flex items-center gap-1.5 px-2.5 py-1.5 border-b border-[var(--border)]">
            <Search size={12} class="text-[var(--text-faint)]" />
            <input
              ref={(el) => (searchInput = el)}
              class="flex-1 bg-transparent text-xs focus:outline-none placeholder:text-[var(--text-faint)]"
              placeholder="搜索模型（名称 / id）…"
              value={query()}
              onInput={(e) => {
                setQuery(e.currentTarget.value);
                setNav(-1);
              }}
              onKeyDown={onSearchKey}
            />
          </div>
          <ModelStatusErrors
            currentError={curErr()}
            globalError={globalErr()}
            onRetryCurrent={() => void reloadCurrent(activeSessionId())}
            onRetryGlobal={() => void reloadGlobal()}
          />
          <div class="max-h-72 overflow-y-auto py-1" ref={(el) => (listEl = el)}>
            <div
              class="model-row"
              classList={{ "model-row-active": following() }}
              onClick={followGlobal}
              onContextMenu={(e) => e.preventDefault()}
            >
              <div class="flex-1 min-w-0">
                <div class="flex items-center gap-1.5">
                  <span class="text-xs font-medium truncate">跟随全局默认</span>
                  <Show when={following()}>
                    <Check size={12} class="text-[var(--accent-hover)]" />
                  </Show>
                </div>
                <div class="text-2xs text-[var(--text-faint)] truncate">
                  当前全局：{globalLabel()}
                </div>
              </div>
            </div>
            <div class="mx-2 my-1 border-t border-[var(--border)]" />
            <Show when={catLoading()}>
              <div class="px-3 py-2 text-2xs text-[var(--text-faint)]">加载模型目录中…</div>
            </Show>
            <Show when={catErr()}>
              <div class="px-3 py-2 text-2xs text-[var(--err)]">
                加载模型目录失败：{catErr()}
                <button
                  class="ml-2 text-[var(--accent-hover)] hover:underline"
                  onClick={() => void reloadCatalog(true)}
                >
                  重试
                </button>
              </div>
            </Show>
            <Show when={!catLoading() && !catErr()}>
              <For each={filtered()}>
                {(r, i) => (
                  <div
                    class="model-row"
                    data-nav={i()}
                    classList={{
                      "model-row-active":
                        r.model.id === cur().model && r.provider === cur().provider,
                      "bg-[var(--bg-overlay)]": i() === nav(),
                    }}
                    onClick={() => pick(r)}
                    onContextMenu={(e) => e.preventDefault()}
                  >
                    <div class="flex-1 min-w-0">
                      <div class="flex items-center gap-1.5">
                        <span class="text-xs font-medium truncate">{r.model.name}</span>
                        <Show when={r.model.reasoning}>
                          <span class="text-2xs px-1 rounded border border-[var(--border)] text-[var(--text-faint)]">
                            推理
                          </span>
                        </Show>
                        <Show when={r.model.modalities_in.some((m) => m !== "text")}>
                          <span class="text-2xs px-1 rounded border border-[var(--border)] text-[var(--text-faint)]">
                            {r.model.modalities_in.filter((m) => m !== "text").join("/")}
                          </span>
                        </Show>
                        <Show when={r.model.id === cur().model && r.provider === cur().provider}>
                          <Check size={12} class="text-[var(--accent-hover)]" />
                        </Show>
                      </div>
                      <div class="text-2xs text-[var(--text-faint)] truncate">
                        {r.providerName} · {r.model.id} · ctx {fmtCtx(r.model.context)}
                      </div>
                    </div>
                  </div>
                )}
              </For>
              <Show when={filtered().length === 0}>
                <div class="px-3 py-2 text-2xs text-[var(--text-faint)]">无匹配模型</div>
              </Show>
            </Show>
          </div>
          <div class="border-t border-[var(--border)] px-2.5 py-1.5">
            <div class="text-2xs text-[var(--text-faint)] mb-1">把当前模型分配为…</div>
            <div class="flex flex-wrap gap-1">
              <For each={ROLE_ASSIGN}>
                {(r) => (
                  <button
                    class="role-chip"
                    disabled={!cur().model}
                    onClick={() => assignRole(r.role, r.label)}
                  >
                    {r.label.replace("设为", "")}
                  </button>
                )}
              </For>
            </div>
            {/* roleMsg 放 popover 内：挂 pill 旁出现/消失会挤压 actionbar 布局 */}
            <Show when={roleMsg()}>
              <div class="text-2xs text-[var(--ok)] mt-1">{roleMsg()}</div>
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}
