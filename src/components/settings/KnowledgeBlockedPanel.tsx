import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import {
  knowledgeAcknowledgeUnknown,
  knowledgeConsolidationBlocked,
  type BlockedConsolidationAttempt,
} from "../../lib/knowledge";
import { flashErr, flashOk } from "../../lib/flash";
import { client } from "../../lib/client";
import { errText } from "../err-text";

export default function KnowledgeBlockedPanel() {
  const [attempts, setAttempts] = createSignal<BlockedConsolidationAttempt[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [loadErr, setLoadErr] = createSignal("");
  const [confirming, setConfirming] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [diagnostics, setDiagnostics] = createSignal<string[]>([]);
  let reloadGeneration = 0;
  let disposed = false;

  const reload = async () => {
    const generation = ++reloadGeneration;
    try {
      const next = await knowledgeConsolidationBlocked();
      if (disposed || generation !== reloadGeneration) return;
      setAttempts(next);
      setLoaded(true);
      setLoadErr("");
    } catch (error) {
      if (disposed || generation !== reloadGeneration) return;
      setLoadErr(errText(error));
    }
  };
  onMount(() => {
    void reload();
    const timer = setInterval(() => {
      if (!busy()) void reload();
    }, 30_000);
    const offResync = client.onResync(() => {
      if (!busy()) void reload();
    });
    onCleanup(() => {
      clearInterval(timer);
      offResync();
    });
  });
  onCleanup(() => {
    disposed = true;
    reloadGeneration++;
  });

  const acknowledge = async (attempt: BlockedConsolidationAttempt) => {
    if (busy()) return;
    reloadGeneration++;
    setDiagnostics([]);
    setBusy(true);
    try {
      const result = await knowledgeAcknowledgeUnknown(attempt.session_id);
      if (disposed) return;
      setConfirming("");
      setDiagnostics(result.diagnostics);
      setAttempts((current) => current.filter((item) => item.session_id !== attempt.session_id));
      await reload();
      if (disposed) return;
      flashOk(
        result.checkpointed_revision === null
          ? "已记录 legacy UNKNOWN；因缺少精确 cursor，当前消息仍会进入下一轮沉淀"
          : "已记录 UNKNOWN 并跳过该快照；只有新消息 cursor 会再次自动沉淀",
      );
    } catch (error) {
      if (disposed) return;
      flashErr(
        `UNKNOWN 确认未完整完成，最终状态 UNKNOWN；attempt 已保留，已重新对账：${errText(error)}`,
      );
      await reload();
    } finally {
      if (!disposed) setBusy(false);
    }
  };

  return (
    <>
      <Show when={loadErr()}>
        <div class="text-xs text-[var(--err)]">
          自动沉淀恢复状态为 UNKNOWN：{loadErr()}
          <button
            class="ml-2 hover:underline disabled:opacity-40"
            disabled={busy()}
            onClick={() => void reload()}
          >
            重试
          </button>
        </div>
      </Show>
      <Show when={diagnostics().length > 0}>
        <div class="text-xs text-[var(--warn)] space-y-1">
          <div>UNKNOWN 处理诊断：</div>
          <For each={diagnostics()}>{(diagnostic) => <div>{diagnostic}</div>}</For>
        </div>
      </Show>
      <Show when={loaded() && attempts().length > 0}>
        <div class="rounded-lg border border-[var(--warn)] bg-[var(--bg-raised)] p-3 space-y-2">
          <div class="text-xs font-medium text-[var(--warn)]">自动沉淀待确认</div>
          <div class="text-2xs text-[var(--text-dim)]">
            Provider 请求可能已发生，但结果未 durable 落盘。系统不会自动重试，避免重复计费。
          </div>
          <For each={attempts()}>
            {(attempt) => (
              <div class="rounded border border-[var(--border)] px-2.5 py-2 space-y-1">
                <div class="text-xs font-mono selectable">{attempt.session_id}</div>
                <div class="text-2xs text-[var(--text-faint)]">{attempt.reason}</div>
                <div class="text-2xs text-[var(--text-faint)]">
                  结果：UNKNOWN · 用量：
                  {attempt.metering_settled
                    ? attempt.usage_unknown
                      ? "UNKNOWN 已记录"
                      : "已结算"
                    : "等待 durable 结算"}
                </div>
                <Show
                  when={confirming() === attempt.session_id}
                  fallback={
                    <button
                      class="pressable px-2 py-1 rounded text-2xs border border-[var(--warn)] text-[var(--warn)]"
                      disabled={busy()}
                      onClick={() => setConfirming(attempt.session_id)}
                    >
                      处理 UNKNOWN
                    </button>
                  }
                >
                  <div class="flex items-center gap-1.5">
                    <button
                      class="pressable px-2 py-1 rounded text-2xs border border-[var(--err)] text-[var(--err)] disabled:opacity-40"
                      disabled={busy()}
                      onClick={() => void acknowledge(attempt)}
                    >
                      确认 UNKNOWN 并跳过快照
                    </button>
                    <button
                      class="pressable px-2 py-1 rounded text-2xs border border-[var(--border)]"
                      disabled={busy()}
                      onClick={() => setConfirming("")}
                    >
                      取消
                    </button>
                  </div>
                </Show>
              </div>
            )}
          </For>
        </div>
      </Show>
    </>
  );
}
