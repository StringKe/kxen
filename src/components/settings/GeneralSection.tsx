import { For, Show } from "solid-js";
import { mode, setMode } from "../../lib/theme";
import UpdateSection from "./UpdateSection";

type ReadinessKey = "workspace" | "provider" | "routing";

export default function GeneralSection(props: {
  readiness: Record<ReadinessKey, boolean | null>;
  sendPolicy: string;
  configLoaded: boolean;
  policySaving: boolean;
  onPolicy: (policy: string) => void;
  onProviders: () => void;
}) {
  return (
    <>
      <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-4 space-y-2">
        <div class="text-sm">首次运行检查</div>
        <For
          each={
            [
              ["workspace", "Workspace 已选择"],
              ["provider", "至少一个 Provider 凭证可用"],
              ["routing", "至少一个角色路由落到可用 Provider"],
            ] as const
          }
        >
          {([key, label]) => (
            <div class="flex items-center gap-2 text-xs">
              <span
                class={
                  props.readiness[key] === true
                    ? "text-[var(--ok)]"
                    : props.readiness[key] === false
                      ? "text-[var(--warn)]"
                      : "text-[var(--text-faint)]"
                }
              >
                {props.readiness[key] === true
                  ? "PASS"
                  : props.readiness[key] === false
                    ? "需要处理"
                    : "UNKNOWN"}
              </span>
              <span class="text-[var(--text-dim)]">{label}</span>
              <Show when={props.readiness[key] === false && key === "provider"}>
                <button
                  class="text-[var(--accent-hover)] hover:underline"
                  onClick={props.onProviders}
                >
                  去配置
                </button>
              </Show>
            </div>
          )}
        </For>
        <div class="text-2xs text-[var(--text-faint)]">
          Shell 命令逐次审批；Browser automation、Remote MCP、自动知识沉淀默认关闭。
        </div>
      </div>
      <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] divide-y divide-[var(--border)]">
        <div class="flex items-center justify-between px-4 py-3">
          <div>
            <div class="text-sm">主题</div>
            <div class="text-xs text-[var(--text-faint)]">跟随系统或手动固定，系统切换实时生效</div>
          </div>
          <div class="flex gap-1">
            <For each={["auto", "dark", "light"] as const}>
              {(theme) => (
                <button
                  class="pressable px-2.5 py-1 rounded-md text-xs border"
                  classList={{
                    "border-[var(--accent)] text-[var(--accent-hover)]": mode() === theme,
                    "border-[var(--border)] text-[var(--text-dim)]": mode() !== theme,
                  }}
                  onClick={() => setMode(theme)}
                >
                  {theme === "auto" ? "跟随系统" : theme === "dark" ? "暗色" : "亮色"}
                </button>
              )}
            </For>
          </div>
        </div>
        <div class="flex items-center justify-between px-4 py-3">
          <div>
            <div class="text-sm">运行中发送</div>
            <div class="text-xs text-[var(--text-faint)]">
              生成中再发消息：排队等当前完成，或打断当前立即发送
            </div>
          </div>
          <div class="flex gap-1">
            <For each={["queue", "interrupt"] as const}>
              {(policy) => (
                <button
                  class="pressable px-2.5 py-1 rounded-md text-xs border"
                  disabled={!props.configLoaded || props.policySaving}
                  classList={{
                    "border-[var(--accent)] text-[var(--accent-hover)]":
                      props.sendPolicy === policy,
                    "border-[var(--border)] text-[var(--text-dim)]": props.sendPolicy !== policy,
                  }}
                  onClick={() => props.onPolicy(policy)}
                >
                  {policy === "queue" ? "排队" : "打断"}
                </button>
              )}
            </For>
          </div>
        </div>
        <UpdateSection />
      </div>
    </>
  );
}
