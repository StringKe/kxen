import { createSignal, For, onMount, Show } from "solid-js";
import { ChevronDown, ChevronRight, Eye, Trash2 } from "lucide-solid";
import EmptyLine from "../EmptyLine";
import CodingRulesBlock from "./CodingRulesBlock";
import {
  knowledgeAdd,
  knowledgeInjectionPreview,
  knowledgeList,
  knowledgeMove,
  knowledgeRemove,
  knowledgeSetEnabled,
  type KnowledgeEntry,
  type KnowledgeKind,
  type KnowledgeScope,
} from "../../lib/knowledge";
import { badgeChip } from "../../lib/variants";
import { activeSessionId } from "../../lib/state";
import { flashErr, flashOk } from "../../lib/flash";
import { errText } from "../err-text";
const SCOPES: { id: KnowledgeScope; label: string; hint: string }[] = [
  { id: "project", label: "项目", hint: ".agents/ · 入 git 共享" },
  { id: "personal", label: "个人", hint: "~/.agents/ · 跨项目" },
];
const KIND_LABELS: Record<KnowledgeKind, string> = {
  rule: "规则",
  reference: "参考",
  skill: "技能",
  command: "命令",
  note: "笔记",
  memory: "记忆",
  history: "历史",
};
const KIND_ORDER: KnowledgeKind[] = [
  "rule",
  "note",
  "memory",
  "reference",
  "skill",
  "command",
  "history",
];
const NOTE_TYPES = ["correction", "convention", "pitfall", "preference", "note"];

export default function KnowledgeSection() {
  const [entries, setEntries] = createSignal<KnowledgeEntry[]>([]);
  const [preview, setPreview] = createSignal<string | null>(null);
  const [showPreview, setShowPreview] = createSignal(false);
  const [scope, setScope] = createSignal<KnowledgeScope>("personal");
  const [noteType, setNoteType] = createSignal("convention");
  const [desc, setDesc] = createSignal("");
  const [content, setContent] = createSignal("");
  const [listLoaded, setListLoaded] = createSignal(false);
  const [listErr, setListErr] = createSignal("");
  const [previewLoaded, setPreviewLoaded] = createSignal(false);
  const [previewErr, setPreviewErr] = createSignal("");
  const [confirmDel, setConfirmDel] = createSignal("");
  const keyOf = (e: KnowledgeEntry) => `${e.scope}:${e.kind}:${e.slug}`;
  let reloadSeq = 0;

  const reload = async () => {
    const seq = ++reloadSeq;
    const [list, prev] = await Promise.allSettled([
      knowledgeList(),
      knowledgeInjectionPreview(activeSessionId() || undefined),
    ]);
    if (seq !== reloadSeq) return;
    if (list.status === "fulfilled") {
      setEntries(list.value);
      setListLoaded(true);
      setListErr("");
    } else {
      setListLoaded(false);
      setListErr(errText(list.reason));
    }
    if (prev.status === "fulfilled") {
      setPreview(prev.value.block ?? null);
      setPreviewLoaded(true);
      setPreviewErr("");
    } else {
      setPreviewLoaded(false);
      setPreviewErr(errText(prev.reason));
    }
  };
  onMount(() => void reload());

  const add = async () => {
    if (!desc().trim() || !content().trim()) return;
    try {
      await knowledgeAdd(scope(), noteType(), desc().trim(), content().trim());
    } catch (e) {
      flashErr(`写入知识库失败：${errText(e)}`);
      return;
    }
    setDesc("");
    setContent("");
    await reload();
    flashOk("已写入知识库");
  };

  const toggle = async (e: KnowledgeEntry) => {
    try {
      await knowledgeSetEnabled(e.scope, e.slug, !e.enabled);
    } catch (err) {
      flashErr(`启停失败：${errText(err)}`);
      return;
    }
    await reload();
  };

  const move = async (e: KnowledgeEntry, to: KnowledgeScope) => {
    if (to === e.scope) return;
    try {
      await knowledgeMove(e.scope, e.slug, to);
    } catch (err) {
      flashErr(`移动失败：${errText(err)}`);
      return;
    }
    await reload();
    flashOk(`已晋升到 ${to === "project" ? "项目" : "个人"}`);
  };

  const remove = async (e: KnowledgeEntry) => {
    try {
      await knowledgeRemove(e.scope, e.slug);
    } catch (err) {
      flashErr(`删除失败：${errText(err)}`);
      setConfirmDel("");
      return;
    }
    setConfirmDel("");
    await reload();
    flashOk("已删除（废纸篓可恢复）"); // 后端 knowledge.remove 进系统废纸篓（store.rs），文案属实
  };

  const byScope = (s: KnowledgeScope) => entries().filter((e) => e.scope === s);
  const byKind = (s: KnowledgeScope, k: KnowledgeKind) => byScope(s).filter((e) => e.kind === k);
  const enabledOf = (s: KnowledgeScope) => byScope(s).filter((e) => e.enabled).length;
  const shadowed = () => {
    const projectSlugs = new Set(byScope("project").map((e) => `${e.kind}:${e.slug}`));
    return new Set(
      byScope("personal")
        .filter((e) => projectSlugs.has(`${e.kind}:${e.slug}`))
        .map((e) => `${e.kind}:${e.slug}`),
    );
  };

  return (
    <>
      <CodingRulesBlock />

      <Show when={listErr()}>
        <div class="text-xs text-[var(--err)]">
          知识列表读取失败，当前结果为 UNKNOWN：{listErr()}
          <button class="ml-2 hover:underline" onClick={() => void reload()}>
            重试
          </button>
        </div>
      </Show>

      <div class="flex items-center justify-between">
        <div class="text-xs text-[var(--text-faint)]">
          {listLoaded()
            ? `项目 ${enabledOf("project")}/${byScope("project").length} · 个人 ${enabledOf("personal")}/${byScope("personal").length}（启用/总条数）`
            : "知识条目统计 UNKNOWN"}
        </div>
        <button
          class="pressable flex items-center gap-1 px-2 py-1 rounded text-xs text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60"
          onClick={() => setShowPreview(!showPreview())}
        >
          <Eye size={12} />
          {showPreview() ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
          注入预览
        </button>
      </div>

      <Show when={showPreview()}>
        <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-3 max-h-72 overflow-auto">
          <div class="text-2xs text-[var(--text-faint)] mb-1.5">
            模型下轮 system prompt 实际看到的知识文本（启停即时生效）
          </div>
          <pre class="selectable text-2xs font-mono whitespace-pre-wrap text-[var(--text-dim)]">
            {previewLoaded()
              ? (preview() ?? "（无注入知识）")
              : previewErr()
                ? `注入预览 UNKNOWN：${previewErr()}`
                : "加载中…"}
          </pre>
        </div>
      </Show>

      <For each={SCOPES}>
        {(s) => (
          <div class="space-y-1.5">
            <div class="flex items-baseline gap-2 px-1">
              <span class="text-xs font-medium">{s.label}</span>
              <span class="text-2xs text-[var(--text-faint)]">{s.hint}</span>
            </div>
            <div class="list-card">
              <For each={KIND_ORDER.filter((k) => byKind(s.id, k).length > 0)}>
                {(k) => (
                  <div class="px-4 py-2.5">
                    <div class="text-2xs text-[var(--text-faint)] mb-1.5">
                      {KIND_LABELS[k]}（{byKind(s.id, k).length}）
                    </div>
                    <div class="space-y-2">
                      <For each={byKind(s.id, k)}>
                        {(e) => (
                          <div
                            class="flex items-start gap-2"
                            classList={{ "opacity-45": !e.enabled }}
                          >
                            <button
                              class="pressable mt-0.5 w-7 h-4 rounded-full relative shrink-0 transition-colors"
                              classList={{
                                "bg-[var(--accent)]": e.enabled,
                                "bg-[var(--bg-overlay)]": !e.enabled,
                              }}
                              title={e.enabled ? "停用（注入即刻跳过）" : "启用"}
                              onClick={() => void toggle(e)}
                            >
                              <span
                                class="absolute top-0.5 w-3 h-3 rounded-full bg-white transition-all"
                                style={e.enabled ? "left:14px" : "left:2px"}
                              />
                            </button>
                            <div class="flex-1 min-w-0">
                              <div class="flex items-center gap-1.5 mb-0.5">
                                <Show when={e.note_type}>
                                  <span class={badgeChip({ tone: "accent" })}>{e.note_type}</span>
                                </Show>
                                <Show
                                  when={
                                    shadowed().has(`${e.kind}:${e.slug}`) && e.scope === "personal"
                                  }
                                >
                                  <span class={badgeChip({ tone: "warn" })}>被项目覆盖</span>
                                </Show>
                                <Show when={e.always_apply}>
                                  <span class={badgeChip({ tone: "faint" })}>always</span>
                                </Show>
                                <span class="text-2xs text-[var(--text-faint)]">{e.date}</span>
                              </div>
                              <div class="text-sm">{e.description}</div>
                              <div
                                class="text-xs text-[var(--text-faint)] truncate"
                                title={e.content}
                              >
                                {e.content}
                              </div>
                            </div>
                            <select
                              class="bg-transparent border border-[var(--border)] rounded px-1 py-0.5 text-2xs text-[var(--text-dim)]"
                              value={e.scope}
                              title="晋升/降级（跨 scope 移动，保 kind）"
                              onChange={(ev) =>
                                void move(e, ev.currentTarget.value as KnowledgeScope)
                              }
                            >
                              {SCOPES.map((sc) => (
                                <option value={sc.id}>{sc.label}</option>
                              ))}
                            </select>
                            <Show
                              when={confirmDel() === keyOf(e)}
                              fallback={
                                <button
                                  class="pressable px-1.5 py-1 rounded text-[var(--text-faint)] hover:text-[var(--err)]"
                                  title="删除（废纸篓可恢复）"
                                  onClick={() => setConfirmDel(keyOf(e))}
                                >
                                  <Trash2 size={13} />
                                </button>
                              }
                            >
                              <span class="flex items-center gap-1">
                                <button
                                  class="pressable px-1.5 py-0.5 rounded text-2xs border border-[var(--err)] text-[var(--err)]"
                                  onClick={() => void remove(e)}
                                >
                                  确认删除
                                </button>
                                <button
                                  class="pressable px-1.5 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                                  onClick={() => setConfirmDel("")}
                                >
                                  取消
                                </button>
                              </span>
                            </Show>
                          </div>
                        )}
                      </For>
                    </div>
                  </div>
                )}
              </For>
              <Show when={listLoaded() && byScope(s.id).length === 0}>
                <EmptyLine text={`暂无${s.label}知识`} />
              </Show>
            </div>
          </div>
        )}
      </For>

      <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-4 space-y-2">
        <div class="text-xs text-[var(--text-faint)]">
          手动添加笔记（Agent 自主沉淀与删除蒸馏共用同一存储）
        </div>
        <div class="flex gap-2">
          <select
            class="form-select"
            value={scope()}
            onChange={(e) => setScope(e.currentTarget.value as KnowledgeScope)}
          >
            <option value="personal">个人（默认）</option>
            <option value="project">项目（克制）</option>
          </select>
          <select
            class="form-select"
            value={noteType()}
            onChange={(e) => setNoteType(e.currentTarget.value)}
          >
            {NOTE_TYPES.map((k) => (
              <option value={k}>{k}</option>
            ))}
          </select>
        </div>
        <input
          class="w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
          placeholder="一句话描述（同题自动生成同 slug 覆盖）"
          value={desc()}
          onInput={(e) => setDesc(e.currentTarget.value)}
        />
        <textarea
          class="w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs h-16"
          placeholder="正文（原子一条，别写流水账）"
          value={content()}
          onInput={(e) => setContent(e.currentTarget.value)}
        />
        <button
          class="pressable px-3 py-1 rounded-md text-xs border border-[var(--border)] disabled:opacity-40"
          disabled={!desc().trim() || !content().trim()}
          onClick={() => void add()}
        >
          写入知识库
        </button>
      </div>
    </>
  );
}
