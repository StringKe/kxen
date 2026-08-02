// MCP server 状态面板（设置页高级区）：状态点 / 交互授权发起与轮询 / 重启。
// 授权等待不空转：后端 finish_auth 成败落 last_auth_error（mcp.status 轮询可见），
// 失败即复位按钮就地显错；轮询与超时兜底句柄统一登记，组件卸载全部清掉。
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { mcpAuth, mcpRestart, mcpStatus, type McpServerStatus } from "../../lib/mcp";
import { formatError } from "../../lib/error-text";
import { flashErr } from "../../lib/flash";
import { writeClipboard } from "../../lib/clipboard";
import { errText } from "../err-text";

export default function McpSection() {
  const [mcpServers, setMcpServers] = createSignal<McpServerStatus[]>([]);
  // OAuth 授权中（等待浏览器回调）与待手动复制的授权 URL（后端没能拉起浏览器时）
  const [authPending, setAuthPending] = createSignal<Record<string, boolean>>({});
  const [authUrls, setAuthUrls] = createSignal<Record<string, string>>({});
  // 最近一次授权失败原因（按 server 名常驻，下一次发起授权时清掉）
  const [authErrs, setAuthErrs] = createSignal<Record<string, string>>({});
  const timers = new Set<ReturnType<typeof setInterval>>();
  onCleanup(() => timers.forEach((t) => clearInterval(t)));

  const refreshMcp = async () => {
    // 轮询场景失败保留旧快照（状态点不闪烁）；重启/授权等用户动作路径已各自显错
    const list = await mcpStatus().catch(() => null);
    if (list) setMcpServers(list);
  };

  const restart = async (name: string) => {
    await mcpRestart(name)
      .then(refreshMcp)
      .catch((e: unknown) => flashErr(`重启失败：${errText(e)}`));
  };

  const startMcpAuth = async (name: string) => {
    setAuthPending((p) => ({ ...p, [name]: true }));
    setAuthErrs((p) => {
      const next = { ...p };
      delete next[name];
      return next;
    });
    const r = await mcpAuth(name).catch((e: unknown) => {
      setAuthErrs((p) => ({ ...p, [name]: errText(e) }));
      return null;
    });
    if (!r) {
      setAuthPending((p) => ({ ...p, [name]: false }));
      return;
    }
    // 浏览器没拉起来：URL 展示出来供手动复制（授权流在后端照常等回调）
    if (!r.opened) setAuthUrls((p) => ({ ...p, [name]: r.authorize_url }));
    const clear = () => {
      setAuthPending((p) => ({ ...p, [name]: false }));
      setAuthUrls((p) => {
        const next = { ...p };
        delete next[name];
        return next;
      });
    };
    const stop = () => {
      clearInterval(timer);
      clearTimeout(cap);
      timers.delete(timer);
      timers.delete(cap);
      clear();
    };
    // 后端完成换 token 会自动重连：轮询直到脱离 needs_auth 或拿到授权失败原因
    const timer = setInterval(() => {
      void refreshMcp().then(() => {
        const cur = mcpServers().find((s) => s.name === name);
        if (!cur) return;
        if (cur.last_auth_error) {
          const reason = cur.last_auth_error;
          stop();
          setAuthErrs((p) => ({ ...p, [name]: reason }));
        } else if (cur.status !== "needs_auth") {
          stop();
        }
      });
    }, 2000);
    // 上限与后端回调超时一致；到点后端仍无结果不能挂死按钮
    const cap = setTimeout(() => {
      stop();
      setAuthErrs((p) => ({ ...p, [name]: "等待授权回调超时，请重试" }));
    }, 300_000);
    timers.add(timer);
    timers.add(cap);
  };

  onMount(() => void refreshMcp());

  return (
    <div>
      <div class="mb-1.5 text-xs text-[var(--text)]">
        MCP servers（.mcp.json / ~/.config/kxen/mcp.json）
      </div>
      <Show when={mcpServers().length > 0} fallback={<div class="text-xs">未配置 MCP server</div>}>
        <For each={mcpServers()}>
          {(s) => (
            <div class="py-1 text-xs">
              <div class="flex items-center gap-2">
                <span
                  class="inline-block w-2 h-2 rounded-full"
                  style={{
                    "background-color":
                      s.status === "running"
                        ? "var(--ok)"
                        : s.status === "needs_auth"
                          ? "var(--warn)"
                          : "var(--err)",
                  }}
                />
                <span class="text-[var(--text)]">{s.name}</span>
                <span class="text-[var(--text-dim)]">{s.transport}</span>
                <Show when={s.url}>
                  {(u) => <span class="truncate text-[var(--text-dim)]">{u()}</span>}
                </Show>
                <span class="text-[var(--text-dim)]">{s.tools} tools</span>
                <Show when={s.resources > 0}>
                  <span class="text-[var(--text-dim)]">{s.resources} resources</span>
                </Show>
                <Show when={s.status === "needs_auth"}>
                  <button
                    class="pressable ml-auto px-2 py-0.5 rounded border border-[var(--warn)] text-[var(--warn)] disabled:opacity-50"
                    disabled={!!authPending()[s.name]}
                    onClick={() => void startMcpAuth(s.name)}
                  >
                    {authPending()[s.name] ? "等待授权…" : "认证"}
                  </button>
                </Show>
                <button
                  class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-[var(--text)]"
                  classList={{ "ml-auto": s.status !== "needs_auth" }}
                  onClick={() => void restart(s.name)}
                >
                  重启
                </button>
              </div>
              <Show when={authErrs()[s.name]}>
                {(e) => (
                  <div class="mt-1 pl-4 text-[var(--err)] break-all">
                    认证失败：{formatError(e())}
                  </div>
                )}
              </Show>
              <Show when={authUrls()[s.name]}>
                {(u) => (
                  <div class="mt-1 flex items-center gap-2 pl-4">
                    <span class="text-[var(--text-dim)]">浏览器未打开，请手动访问：</span>
                    <code class="flex-1 truncate text-[var(--text)] select-all">{u()}</code>
                    <button
                      class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-[var(--text)]"
                      onClick={() => writeClipboard(u())}
                    >
                      复制
                    </button>
                  </div>
                )}
              </Show>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
