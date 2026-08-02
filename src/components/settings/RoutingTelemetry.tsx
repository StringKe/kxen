import { For, Show } from "solid-js";
import type { DispatchRecord, MrmHealth } from "../../lib/provider";
import { hasUnknownUsage, usageUnknownDetail } from "../../lib/usage";
import type { Slot } from "./routing";

export function RoutingTelemetry(props: { slots: Slot[]; health: MrmHealth[] }) {
  return (
    <>
      <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] px-4 py-3">
        <div class="text-xs text-[var(--text-faint)] mb-2">并发槽位</div>
        <div class="space-y-1.5">
          <For
            each={props.slots}
            fallback={<div class="text-xs text-[var(--text-faint)]">无运行中派发</div>}
          >
            {(slot) => (
              <div class="flex items-center gap-2 text-xs">
                <span class="w-24 text-[var(--text-dim)]">{slot.provider}</span>
                <span class="ctx-bar flex-1">
                  <span
                    class="ctx-bar-fill"
                    style={`width:${(slot.available / slot.limit) * 100}%`}
                  />
                </span>
                <span class="tabular-nums text-[var(--text-faint)]">
                  {slot.available}/{slot.limit}
                </span>
              </div>
            )}
          </For>
        </div>
      </div>
      <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] px-4 py-3">
        <div class="text-xs text-[var(--text-faint)] mb-2">Provider 健康与今日预算</div>
        <div class="space-y-1.5">
          <For
            each={props.health}
            fallback={
              <div class="text-xs text-[var(--text-faint)]">暂无 Provider 用量或熔断记录</div>
            }
          >
            {(item) => (
              <div class="flex items-center gap-3 text-xs">
                <span class="w-24 truncate text-[var(--text-dim)]">{item.provider}</span>
                <span class={item.circuit_open ? "text-[var(--err)]" : "text-[var(--ok)]"}>
                  {item.circuit_open
                    ? `熔断 ${item.cooldown_remaining_seconds}s`
                    : `连续失败 ${item.consecutive_failures}`}
                </span>
                <span
                  class="text-[var(--text-faint)] tabular-nums"
                  title={hasUnknownUsage(item) ? usageUnknownDetail(item) : undefined}
                >
                  今日 {hasUnknownUsage(item) ? "≥" : ""}
                  {item.today_input + item.today_output} tokens
                  <Show when={hasUnknownUsage(item)}>
                    <span class="ml-1 text-[var(--warn)]">
                      UNKNOWN
                      {(item.unmetered_calls ?? 0) > 0
                        ? `，${item.unmetered_calls} 次无法计量`
                        : ""}
                    </span>
                  </Show>
                </span>
                <span class="ml-auto text-[var(--text-faint)] tabular-nums">
                  {item.estimated_cost_usd == null
                    ? "金额 UNKNOWN"
                    : `$${item.estimated_cost_usd.toFixed(4)}${
                        item.daily_cost_budget_usd == null
                          ? ""
                          : ` / $${item.daily_cost_budget_usd.toFixed(4)}`
                      }`}
                </span>
              </div>
            )}
          </For>
        </div>
      </div>
    </>
  );
}

export function RoutingHistory(props: { history: DispatchRecord[] }) {
  return (
    <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)]">
      <div class="px-4 py-2 border-b border-[var(--border)] text-xs text-[var(--text-faint)]">
        最近派发
      </div>
      <div class="divide-y divide-[var(--border)]">
        <For
          each={props.history}
          fallback={<div class="px-4 py-3 text-xs text-[var(--text-faint)]">暂无派发记录</div>}
        >
          {(item) => (
            <div class="px-4 py-2 flex items-center gap-3 text-xs">
              <span class="w-20 text-[var(--text-dim)]">{item.role}</span>
              <span class="font-mono flex-1 truncate">
                {item.provider}/{item.model}
              </span>
              <Show when={item.degraded_from}>
                <span class="text-2xs text-[var(--warn)]">降级</span>
              </Show>
              <span class="text-2xs text-[var(--text-faint)] tabular-nums">
                {new Date(item.at).toLocaleTimeString("zh-CN", { hour12: false })}
              </span>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
