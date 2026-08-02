// 诊断区：doctor RPC 结构化呈现（凭证 + MCP/LSP/MRM/event bus），与 /doctor markdown 同数据源。
import { createSignal, For, onMount, Show } from "solid-js";
import { doctor, type DoctorReport } from "../../lib/chat";

const STATUS_TEXT: Record<string, string> = {
  ok: "正常",
  imported: "已导入",
  expired: "已过期",
  missing: "未配置",
  running: "运行中",
  down: "不可用",
  needs_auth: "待授权",
};

function tone(status: string): string {
  if (["ok", "imported", "running"].includes(status)) return "text-[var(--ok)]";
  if (["expired", "needs_auth"].includes(status)) return "text-[var(--warn)]";
  return "text-[var(--text-faint)]";
}

export default function DoctorSection() {
  const [report, setReport] = createSignal<DoctorReport | null>(null);
  const [failed, setFailed] = createSignal(false);

  onMount(async () => {
    try {
      setReport(await doctor());
    } catch {
      setFailed(true);
    }
  });

  return (
    <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-4 space-y-4 text-sm text-[var(--text-dim)]">
      <Show when={failed()}>
        <div class="text-xs text-[var(--err)]">诊断数据加载失败（后端未连接）</div>
      </Show>
      <Show when={!failed() && !report()}>
        <div class="text-xs text-[var(--text-faint)]">加载中…</div>
      </Show>
      <Show when={report()}>
        {(r) => (
          <>
            <div class="text-2xs text-[var(--text-faint)]">
              版本 {r().runtime} · 数据目录 {r().data_dir} · 配置目录 {r().config_dir}
            </div>

            <section>
              <div class="overline">账号凭证</div>
              <div class="space-y-1">
                <For each={r().entries}>
                  {(e) => (
                    <div class="flex items-center gap-2 text-xs">
                      <span class={`shrink-0 ${tone(e.status)}`}>
                        {STATUS_TEXT[e.status] ?? e.status}
                      </span>
                      <span class="text-[var(--text)]">{e.display}</span>
                      <Show when={e.status === "expired"}>
                        <span class="text-2xs text-[var(--text-faint)]">下次调用自动刷新</span>
                      </Show>
                    </div>
                  )}
                </For>
              </div>
            </section>

            <Show when={r().system}>
              {(s) => (
                <>
                  <section>
                    <div class="overline">MCP Servers</div>
                    <Show
                      when={s().mcp_ready}
                      fallback={
                        <div class="text-xs text-[var(--text-faint)]">
                          当前 Workspace 的 MCP runtime 尚未加载，状态 UNKNOWN
                        </div>
                      }
                    >
                      <Show
                        when={s().mcp.length > 0}
                        fallback={<div class="text-xs text-[var(--text-faint)]">未配置</div>}
                      >
                        <div class="space-y-1">
                          <For each={s().mcp}>
                            {(m) => (
                              <div class="flex items-center gap-2 text-xs">
                                <span class={`shrink-0 ${tone(m.status)}`}>
                                  {STATUS_TEXT[m.status] ?? m.status}
                                </span>
                                <span class="text-[var(--text)]">{m.name}</span>
                                <span class="text-2xs text-[var(--text-faint)]">
                                  {m.transport} · {m.tools} 工具 · {m.resources} 资源
                                </span>
                              </div>
                            )}
                          </For>
                        </div>
                      </Show>
                    </Show>
                  </section>

                  <section>
                    <div class="overline">LSP（root：{s().lsp_root}）</div>
                    <Show
                      when={s().lsp.length > 0}
                      fallback={
                        <div class="text-xs text-[var(--text-faint)]">
                          无已触发实例（懒启动：未触发 = 状态未知）
                        </div>
                      }
                    >
                      <div class="space-y-1">
                        <For each={s().lsp}>
                          {(l) => (
                            <div class="flex items-center gap-2 text-xs">
                              <span class={`shrink-0 ${tone(l.status)}`}>
                                {STATUS_TEXT[l.status] ?? l.status}
                              </span>
                              <span class="text-[var(--text)]">{l.language}</span>
                            </div>
                          )}
                        </For>
                      </div>
                    </Show>
                  </section>

                  <section>
                    <div class="overline">
                      MRM 模型调度（当前进程最近路由解析记录 {s().mrm_dispatches} 条）
                    </div>
                    <pre class="text-2xs font-mono whitespace-pre-wrap text-[var(--text-dim)] bg-[var(--bg-overlay)]/40 rounded p-2">
                      {s().mrm_describe}
                    </pre>
                  </section>

                  <section>
                    <div class="overline">Event Bus</div>
                    <div class="text-xs">
                      容量 {s().bus_capacity} · 活跃订阅 {s().bus_receivers}
                      <Show when={s().bus_receivers === 0}>
                        <span class="text-[var(--err)]">（异常：无订阅者，事件全在丢）</span>
                      </Show>
                    </div>
                  </section>
                </>
              )}
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}
