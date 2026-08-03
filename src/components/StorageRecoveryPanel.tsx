import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { client } from "../lib/client";
import { flashErr, flashOk } from "../lib/flash";
import {
  clearStorageRecoveryBlock,
  inspectStorageRecovery,
  repairStorageRecovery,
  type StorageRecoveryReport,
} from "../lib/recovery";
import { errText } from "./err-text";

function isHealthy(report: StorageRecoveryReport): boolean {
  const messagesHealthy = report.session.messages.status === "healthy";
  const queueHealthy =
    report.queue.integrity.status === "healthy" || report.queue.integrity.status === "missing";
  return messagesHealthy && queueHealthy && !report.session.blocked && !report.queue.blocked;
}

function messageSummary(report: StorageRecoveryReport): string {
  const integrity = report.session.messages;
  if (integrity.status === "healthy") return `消息日志完整，共 ${integrity.records} 条记录`;
  if (integrity.status === "corrupt") {
    return `消息日志第 ${integrity.line} 行损坏：${integrity.error}`;
  }
  return integrity.preserve_final_record
    ? `消息日志缺少结尾换行；${integrity.records} 条前序记录和最后一条完整记录可保留`
    : `消息日志尾部不完整；可保留 ${integrity.records} 条完整记录，不完整尾部将从工作副本移除`;
}

function queueSummary(report: StorageRecoveryReport): string {
  const integrity = report.queue.integrity;
  if (integrity.status === "missing") return "待发送队列为空";
  if (integrity.status === "healthy") return `待发送队列完整，共 ${integrity.deliveries} 条投递`;
  return `待发送队列损坏：${integrity.error}`;
}

export default function StorageRecoveryPanel(props: {
  sessionId: () => string;
  onBlockedChange?: (blocked: boolean) => void;
  onRecovered?: () => void;
}) {
  const [report, setReport] = createSignal<StorageRecoveryReport | null>(null);
  const [loadError, setLoadError] = createSignal("");
  const [checking, setChecking] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [confirmingTail, setConfirmingTail] = createSignal(false);
  let loadGeneration = 0;
  let actionGeneration = 0;
  let requestedSessionId = "";
  let inspection: Promise<void> | null = null;

  const inspectOnce = async (sessionId: string) => {
    const generation = ++loadGeneration;
    setChecking(true);
    try {
      const next = await inspectStorageRecovery(sessionId);
      if (generation === loadGeneration && props.sessionId() === sessionId) {
        setReport(next);
        setLoadError("");
        setChecking(false);
      }
    } catch (error) {
      if (generation === loadGeneration && props.sessionId() === sessionId) {
        setLoadError(errText(error));
        setChecking(false);
      }
    }
  };

  const reload = (sessionId = props.sessionId()): Promise<void> => {
    requestedSessionId = sessionId;
    if (inspection) return inspection;
    inspection = (async () => {
      while (requestedSessionId) {
        const nextSessionId = requestedSessionId;
        requestedSessionId = "";
        await inspectOnce(nextSessionId);
      }
    })().finally(() => {
      inspection = null;
    });
    return inspection;
  };

  const refreshIfIdle = () => {
    if (props.sessionId() && !busy()) void reload();
  };

  createEffect(() => {
    const sessionId = props.sessionId();
    loadGeneration++;
    actionGeneration++;
    setReport(null);
    setLoadError("");
    setChecking(Boolean(sessionId));
    setBusy(false);
    setConfirmingTail(false);
    if (sessionId) void reload(sessionId);
  });
  onCleanup(() => {
    loadGeneration++;
    actionGeneration++;
  });
  onMount(() => {
    const timer = setInterval(refreshIfIdle, 30_000);
    const offUpdate = client.stream<{ session_id?: string }>("session.update").on((event) => {
      if (!event.session_id || event.session_id === props.sessionId()) refreshIfIdle();
    });
    const offResync = client.onResync(refreshIfIdle);
    onCleanup(() => {
      clearInterval(timer);
      offUpdate();
      offResync();
    });
  });

  const recover = async () => {
    const current = report();
    const sessionId = props.sessionId();
    if (!current || !sessionId || busy()) return;
    const generation = ++actionGeneration;
    ++loadGeneration;
    setChecking(false);
    setBusy(true);
    try {
      const next =
        current.session.messages.status === "healthy"
          ? await clearStorageRecoveryBlock(sessionId)
          : await repairStorageRecovery(sessionId);
      if (generation !== actionGeneration || props.sessionId() !== sessionId) return;
      setReport(next);
      setLoadError("");
      setConfirmingTail(false);
      if (isHealthy(next)) {
        props.onRecovered?.();
        flashOk(
          next.session.evidence_path
            ? `存储恢复完成；原始消息日志已备份到 ${next.session.evidence_path}`
            : "存储一致性已验证，写入阻塞已解除",
        );
      } else {
        flashErr("存储恢复请求已返回，但阻塞尚未全部解除；请按当前检查结果继续处理");
      }
    } catch (error) {
      if (generation !== actionGeneration || props.sessionId() !== sessionId) return;
      await reload(sessionId);
      const inspected = report();
      if (inspected && isHealthy(inspected) && !loadError()) {
        props.onRecovered?.();
        flashErr(`存储恢复未完整返回，但重新检查后状态已恢复：${errText(error)}`);
      } else {
        flashErr(`存储恢复未完整完成，重新检查后最终状态仍为 UNKNOWN：${errText(error)}`);
      }
    } finally {
      if (generation === actionGeneration && props.sessionId() === sessionId) setBusy(false);
    }
  };

  const visibleReport = () => {
    const current = report();
    return current && !isHealthy(current) ? current : null;
  };
  const canRecover = () => {
    const current = visibleReport();
    return Boolean(current?.session.repairable && current.queue.repairable);
  };
  const tailRepair = () => visibleReport()?.session.messages.status === "repairable_tail";
  createEffect(() =>
    props.onBlockedChange?.(
      Boolean(visibleReport()) || busy() || Boolean(loadError()) || (checking() && !report()),
    ),
  );
  onCleanup(() => props.onBlockedChange?.(false));

  return (
    <>
      <Show when={loadError()}>
        <div class="mb-2 rounded-lg border border-[var(--err)]/50 bg-[var(--err)]/5 px-3 py-2.5 text-xs">
          <div class="text-[var(--err)]">无法确认会话存储状态：{loadError()}</div>
          <button
            class="pressable mt-2 px-2 py-1 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
            disabled={checking() || busy()}
            onClick={() => void reload()}
          >
            重新检查
          </button>
        </div>
      </Show>
      <Show when={visibleReport()}>
        {(current) => (
          <div class="mb-2 rounded-lg border border-[var(--warn)]/50 bg-[var(--warn)]/5 px-3 py-2.5 text-xs space-y-2">
            <div class="font-medium text-[var(--warn)]">会话存储需要恢复</div>
            <div class="text-[var(--text-dim)]">{messageSummary(current())}</div>
            <div class="text-[var(--text-dim)]">{queueSummary(current())}</div>
            <Show when={current().session.blocked}>
              <div class="text-2xs text-[var(--text-faint)]">
                消息写入阻塞：{current().session.blocked}
              </div>
            </Show>
            <Show when={current().queue.blocked}>
              <div class="text-2xs text-[var(--text-faint)]">
                队列写入阻塞：{current().queue.blocked}
              </div>
            </Show>
            <Show when={!canRecover()}>
              <div class="text-2xs text-[var(--err)]">
                自动恢复已 fail closed。请先导出诊断包，再人工核对存储文件与恢复证据。
              </div>
            </Show>
            <Show
              when={tailRepair() && confirmingTail()}
              fallback={
                <div class="flex gap-2">
                  <Show when={canRecover()}>
                    <button
                      class="pressable px-2.5 py-1 rounded text-2xs bg-[var(--accent)] text-[var(--accent-contrast)] disabled:opacity-50"
                      disabled={busy()}
                      onClick={() => (tailRepair() ? setConfirmingTail(true) : void recover())}
                    >
                      {tailRepair() ? "审查并修复日志尾部" : "验证并解除阻塞"}
                    </button>
                  </Show>
                  <button
                    class="pressable px-2.5 py-1 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)] disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => void reload()}
                  >
                    重新检查
                  </button>
                </div>
              }
            >
              <div class="rounded border border-[var(--warn)]/50 p-2 space-y-2">
                <div class="text-[var(--warn)]">
                  修复前会以 0600 权限完整备份原始 JSONL。无效的最后一段字节不会进入工作副本。
                </div>
                <div class="flex gap-2">
                  <button
                    class="pressable px-2.5 py-1 rounded text-2xs bg-[var(--accent)] text-[var(--accent-contrast)] disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => void recover()}
                  >
                    确认备份并修复
                  </button>
                  <button
                    class="pressable px-2.5 py-1 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)] disabled:opacity-50"
                    disabled={busy()}
                    onClick={() => setConfirmingTail(false)}
                  >
                    取消
                  </button>
                </div>
              </div>
            </Show>
          </div>
        )}
      </Show>
    </>
  );
}
