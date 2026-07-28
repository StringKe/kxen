import { createSignal, For, onMount, Show } from "solid-js";
import { A } from "@solidjs/router";
import { ArrowLeft } from "lucide-solid";
import KnowledgeSection from "../components/settings/KnowledgeSection";
import DoctorSection from "../components/settings/DoctorSection";
import McpSection from "../components/settings/McpSection";
import ProvidersSection from "../components/settings/ProvidersSection";
import RoutingSection from "../components/settings/RoutingSection";
import ScheduleSection from "../components/settings/ScheduleSection";
import UsageSection from "../components/settings/UsageSection";
import VoiceSection from "../components/settings/VoiceSection";
import UpdateSection from "../components/settings/UpdateSection";
import { client } from "../lib/client";
import { configGet, currentModel, doctor } from "../lib/chat";
import { flashErr, flashOk } from "../lib/flash";
import { onDragStart } from "../lib/drag";
import { mode, setMode } from "../lib/theme";

const SECTIONS = [
  "通用",
  "提供商",
  "语音",
  "模型路由",
  "用量与统计",
  "知识库 OKF",
  "定时任务",
  "诊断",
  "高级",
] as const;

export default function Settings() {
  const [section, setSection] = createSignal<(typeof SECTIONS)[number]>("通用");
  const [sendPolicy, setSendPolicy] = createSignal("queue");
  const [experimental, setExperimental] = createSignal({
    automatic_knowledge_distillation: false,
    browser_automation: false,
    remote_mcp: false,
  });
  const [readiness, setReadiness] = createSignal({
    workspace: false,
    provider: false,
    routing: false,
  });
  const [distillModel, setDistillModel] = createSignal("当前默认 Provider");

  onMount(async () => {
    // 首屏读取失败保持缺省 queue 不阻塞页面；用户改动时的保存路径才显错
    const [cfg, report, model] = await Promise.all([
      configGet().catch(() => null),
      doctor().catch(() => null),
      currentModel().catch(() => null),
    ]);
    if (model?.provider && model?.model) {
      setDistillModel(`${model.provider}/${model.model}`);
    }
    if (cfg?.send_when_running) setSendPolicy(cfg.send_when_running);
    if (cfg?.experimental) {
      setExperimental({
        automatic_knowledge_distillation:
          cfg.experimental.automatic_knowledge_distillation === true,
        browser_automation: cfg.experimental.browser_automation === true,
        remote_mcp: cfg.experimental.remote_mcp === true,
      });
    }
    const availableProviders = new Set(
      report?.entries
        ?.filter((entry) => ["ok", "imported"].includes(entry.status))
        .map((entry) => entry.provider) ?? [],
    );
    setReadiness({
      workspace: Boolean(report?.system?.lsp_root),
      provider: availableProviders.size > 0,
      routing: Object.values(cfg?.roles ?? {}).some((binding) =>
        availableProviders.has(binding.provider),
      ),
    });
  });

  const setPolicy = async (p: string) => {
    const prev = sendPolicy();
    setSendPolicy(p);
    await client.rpc("config.set_send_policy", { policy: p }).catch((e: unknown) => {
      setSendPolicy(prev); // 乐观更新失败回滚，不留假状态
      flashErr(`保存失败：${e instanceof Error ? e.message : String(e)}`);
    });
  };

  type ExperimentalKey = keyof ReturnType<typeof experimental>;
  const setExperimentalFlag = async (key: ExperimentalKey, enabled: boolean) => {
    const prev = experimental();
    setExperimental({ ...prev, [key]: enabled });
    await client.rpc("config.set_experimental", { key, enabled }).catch((e: unknown) => {
      setExperimental(prev);
      flashErr(`保存失败：${e instanceof Error ? e.message : String(e)}`);
    });
  };

  const exportDiag = async () => {
    const r = await client.rpc<{ path: string }>("diagnostics.export").catch(() => null);
    if (r) flashOk(`已导出 ${r.path}`);
    else flashErr("导出诊断包失败");
  };

  return (
    <div class="h-full flex-1 overflow-auto">
      <div class="h-8" data-tauri-drag-region onMouseDown={onDragStart} />
      <div class="px-8 py-6 pt-2 flex gap-8">
        <nav class="w-36 shrink-0 space-y-0.5">
          <A
            href="/"
            class="flex items-center gap-1.5 text-xs text-[var(--text-dim)] hover:text-[var(--text)] mb-3"
          >
            <ArrowLeft size={13} />
            返回会话
          </A>
          {SECTIONS.map((s) => (
            <button
              class="w-full text-left px-2.5 py-1.5 rounded-md text-sm"
              classList={{
                "bg-[var(--bg-overlay)] text-[var(--text)]": section() === s,
                "text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60": section() !== s,
              }}
              onClick={() => setSection(s)}
            >
              {s}
            </button>
          ))}
        </nav>

        <div class="flex-1 min-w-0 space-y-4">
          <Show when={section() === "通用"}>
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
                    <span class={readiness()[key] ? "text-[var(--ok)]" : "text-[var(--warn)]"}>
                      {readiness()[key] ? "PASS" : "需要处理"}
                    </span>
                    <span class="text-[var(--text-dim)]">{label}</span>
                    <Show when={!readiness()[key] && key === "provider"}>
                      <button
                        class="text-[var(--accent-hover)] hover:underline"
                        onClick={() => setSection("提供商")}
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
                  <div class="text-xs text-[var(--text-faint)]">
                    跟随系统或手动固定，系统切换实时生效
                  </div>
                </div>
                <div class="flex gap-1">
                  <For each={["auto", "dark", "light"] as const}>
                    {(m) => (
                      <button
                        class="pressable px-2.5 py-1 rounded-md text-xs border"
                        classList={{
                          "border-[var(--accent)] text-[var(--accent-hover)]": mode() === m,
                          "border-[var(--border)] text-[var(--text-dim)]": mode() !== m,
                        }}
                        onClick={() => setMode(m)}
                      >
                        {m === "auto" ? "跟随系统" : m === "dark" ? "暗色" : "亮色"}
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
                    {(p) => (
                      <button
                        class="pressable px-2.5 py-1 rounded-md text-xs border"
                        classList={{
                          "border-[var(--accent)] text-[var(--accent-hover)]": sendPolicy() === p,
                          "border-[var(--border)] text-[var(--text-dim)]": sendPolicy() !== p,
                        }}
                        onClick={() => void setPolicy(p)}
                      >
                        {p === "queue" ? "排队" : "打断"}
                      </button>
                    )}
                  </For>
                </div>
              </div>
              <UpdateSection />
            </div>
          </Show>

          <Show when={section() === "提供商"}>
            <ProvidersSection />
          </Show>

          <Show when={section() === "模型路由"}>
            <RoutingSection />
          </Show>

          <Show when={section() === "语音"}>
            <VoiceSection />
          </Show>

          <Show when={section() === "用量与统计"}>
            <UsageSection />
          </Show>

          <Show when={section() === "知识库 OKF"}>
            <KnowledgeSection />
          </Show>

          <Show when={section() === "定时任务"}>
            <ScheduleSection />
          </Show>

          <Show when={section() === "诊断"}>
            <DoctorSection />
          </Show>

          <Show when={section() === "高级"}>
            <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-4 space-y-3 text-sm text-[var(--text-dim)]">
              <div class="space-y-2">
                <div class="text-sm text-[var(--text)]">实验能力与数据边界</div>
                <div class="text-xs text-[var(--text-faint)]">
                  模型调用会把你的消息、显式附件、注入知识和工具结果发送给当前会话显示的
                  Provider。以下能力会扩大自动外发或网络访问范围，默认全部关闭。
                </div>
                <For
                  each={
                    [
                      [
                        "automatic_knowledge_distillation",
                        "自动知识沉淀",
                        `每 30 分钟把近 24 小时活跃 Session 各自最近 20 条文本和注入上下文发送给 ${distillModel()}，只写个人知识库`,
                      ],
                      [
                        "browser_automation",
                        "Browser automation",
                        "页面后续导航和全部子资源尚不能形成完整 SSRF 边界",
                      ],
                      [
                        "remote_mcp",
                        "Remote MCP",
                        "允许 HTTP/SSE MCP server 接收工具参数；本地 stdio MCP 不受影响",
                      ],
                    ] as const
                  }
                >
                  {([key, label, hint]) => (
                    <div class="flex items-center justify-between gap-4 py-1.5">
                      <div>
                        <div class="text-xs text-[var(--text)]">{label}</div>
                        <div class="text-2xs text-[var(--text-faint)]">{hint}</div>
                      </div>
                      <button
                        class="pressable px-2.5 py-1 rounded-md text-xs border"
                        classList={{
                          "border-[var(--warn)] text-[var(--warn)]": experimental()[key],
                          "border-[var(--border)] text-[var(--text-dim)]": !experimental()[key],
                        }}
                        onClick={() => void setExperimentalFlag(key, !experimental()[key])}
                      >
                        {experimental()[key] ? "已启用" : "已关闭"}
                      </button>
                    </div>
                  )}
                </For>
              </div>
              <div class="pt-2 border-t border-[var(--border)]">
                <div>
                  hooks：`~/.config/kxen/config.toml` 的 [hooks]（默认全关，pre_tool_use 可阻断）
                </div>
                <div>statusline：同文件 [statusline] items 白名单控制显隐</div>
              </div>
              <div class="pt-2 border-t border-[var(--border)]">
                <McpSection />
              </div>
              <div class="pt-1 border-t border-[var(--border)] flex items-center gap-3">
                <button
                  class="pressable px-3 py-1.5 rounded-md text-xs border border-[var(--border)] text-[var(--text)]"
                  onClick={() => void exportDiag()}
                >
                  导出诊断包（markdown）
                </button>
              </div>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}
