// 内置编码规则区块：app 自带规则的启停开关 + 全文展开（注入所有会话，含 subagent）。
import { createSignal, onMount, Show } from "solid-js";
import { ChevronDown, ChevronRight } from "lucide-solid";
import { codingRulesGet, codingRulesSet, type CodingRulesInfo } from "../../lib/knowledge";
import { flashErr } from "../../lib/flash";
import { errText } from "../err-text";

export default function CodingRulesBlock() {
  const [codingRules, setCodingRules] = createSignal<CodingRulesInfo | null>(null);
  const [showRules, setShowRules] = createSignal(false);
  const [loadErr, setLoadErr] = createSignal("");
  const [saving, setSaving] = createSignal(false);

  const load = async () => {
    try {
      setCodingRules(await codingRulesGet());
      setLoadErr("");
    } catch (error) {
      setLoadErr(errText(error));
    }
  };
  onMount(() => void load());

  const toggle = async () => {
    const cur = codingRules();
    if (!cur || saving()) return;
    setSaving(true);
    try {
      await codingRulesSet(!cur.enabled);
    } catch (e) {
      flashErr(`内置规则启停失败：${errText(e)}`);
      return;
    } finally {
      setSaving(false);
    }
    setCodingRules((current) => (current ? { ...current, enabled: !cur.enabled } : current));
  };

  return (
    <>
      <Show when={loadErr()}>
        <div class="text-xs text-[var(--err)]">
          内置编码规则读取失败，状态 UNKNOWN：{loadErr()}
          <button class="ml-2 hover:underline" onClick={() => void load()}>
            重试
          </button>
        </div>
      </Show>
      <Show when={codingRules()}>
        {(rules) => (
          <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-3 space-y-2">
            <div class="flex items-center gap-2">
              <button
                class="pressable w-7 h-4 rounded-full relative shrink-0 transition-colors"
                classList={{
                  "bg-[var(--accent)]": rules().enabled,
                  "bg-[var(--bg-overlay)]": !rules().enabled,
                }}
                title={rules().enabled ? "停用（下轮起不再注入）" : "启用"}
                disabled={saving()}
                onClick={() => void toggle()}
              >
                <span
                  class="absolute top-0.5 w-3 h-3 rounded-full bg-white transition-all"
                  style={rules().enabled ? "left:14px" : "left:2px"}
                />
              </button>
              <span class="text-xs font-medium">内置编码规则</span>
              <span class="text-2xs text-[var(--text-faint)]">
                app 自带 · 注入所有会话（含 subagent）
              </span>
              <button
                class="pressable ml-auto flex items-center gap-1 px-2 py-1 rounded text-xs text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60"
                onClick={() => setShowRules(!showRules())}
              >
                {showRules() ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
                全文
              </button>
            </div>
            <Show when={showRules()}>
              <pre class="selectable text-2xs font-mono whitespace-pre-wrap text-[var(--text-dim)] max-h-72 overflow-auto">
                {rules().content}
              </pre>
            </Show>
          </div>
        )}
      </Show>
    </>
  );
}
