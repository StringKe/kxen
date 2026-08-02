// 语音区：引擎状态/切换 + 降级链编辑 + locale + 转写 provider key 配置。
// 引擎清单与转写 key 列表全部来自 voice.engines 总览（后端 registry），前端不硬编码 openai/xai。
import { createSignal, For, onMount, Show } from "solid-js";
import {
  setVoiceEngine,
  setVoiceProviderKey,
  voiceEngines,
  type VoiceOverview,
} from "../../lib/voice";
import { flashErr, flashOk } from "../../lib/flash";
import { errText } from "../err-text";

const BADGE: Record<string, { text: string; cls: string }> = {
  ready: { text: "就绪", cls: "text-[var(--ok)]" },
  needs_auth: { text: "待授权", cls: "text-[var(--warn)]" },
  unconfigured: { text: "未配置", cls: "text-[var(--warn)]" },
  unavailable: { text: "不可用", cls: "text-[var(--err)]" },
};

// Apple Speech 常用识别语言；config.toml 的 [voice] locale 是同一键
const LOCALES = ["zh-CN", "zh-HK", "en-US", "ja-JP", "ko-KR"];

export default function VoiceSection() {
  const [ov, setOv] = createSignal<VoiceOverview | null>(null);
  const [loadErr, setLoadErr] = createSignal("");
  const [keys, setKeys] = createSignal<Record<string, string>>({});

  const reload = async () => {
    const r = await voiceEngines().catch((e: unknown) => {
      setLoadErr(errText(e)); // 失败显错误态，不留裸空白
      return null;
    });
    if (r) {
      setOv(r);
      setLoadErr("");
    }
  };
  onMount(() => void reload());

  /** 写引擎配置的统一出口：engine/fallback/locale 一次 merge 落盘并热生效。 */
  const saveEngine = async (patch: { engine?: string; fallback?: string[]; locale?: string }) => {
    const cur = ov();
    if (!cur) return;
    try {
      await setVoiceEngine(
        patch.engine ?? cur.engine,
        patch.fallback ?? cur.fallback,
        patch.locale ?? cur.locale,
      );
      await reload();
      flashOk("语音配置已保存并热生效");
    } catch (e) {
      flashErr(`保存语音配置失败：${errText(e)}`);
    }
  };

  const switchEngine = (engine: string) => void saveEngine({ engine });
  const setLocale = (locale: string) => void saveEngine({ locale });

  // 降级链 = 主引擎失败时按 engines 列表序尝试的勾选项；当前主引擎不可入链
  const inFallback = (id: string) => (ov()?.fallback ?? []).includes(id);
  const toggleFallback = (id: string) => {
    const cur = ov();
    if (!cur) return;
    const next = inFallback(id) ? cur.fallback.filter((f) => f !== id) : [...cur.fallback, id];
    void saveEngine({ fallback: next });
  };

  const saveKey = async (provider: string) => {
    const key = (keys()[provider] ?? "").trim();
    if (!key) return;
    try {
      await setVoiceProviderKey(provider, key);
      setKeys((prev) => ({ ...prev, [provider]: "" }));
      await reload();
      flashOk(`${provider} 转写 key 已保存`);
    } catch (e) {
      flashErr(`保存 ${provider} 转写 key 失败：${errText(e)}`);
    }
  };

  // 转写 key 列表按后端总览动态列（registry 有转写能力的 provider）；
  // custom:* 的 key 随自定义提供商保存时写入，不在此重复配置
  const keyProviders = () =>
    (ov()?.engines ?? []).filter((e) => e.id !== "apple" && !e.id.startsWith("custom:"));

  return (
    <>
      <div class="text-xs text-[var(--text-faint)]">
        主引擎 Apple 本地识别（离线零成本）；provider 转写为可切换引擎与降级链
      </div>
      <Show when={loadErr()}>
        <div class="rounded border border-[var(--err)]/50 bg-[var(--err)]/5 px-3 py-2 text-xs flex items-center gap-3">
          <span class="text-[var(--err)]">加载语音配置失败：{loadErr()}</span>
          <button
            class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-[var(--text-dim)]"
            onClick={() => void reload()}
          >
            重试
          </button>
        </div>
      </Show>
      <Show when={!ov() && !loadErr()}>
        <div class="text-xs text-[var(--text-faint)]">加载中…</div>
      </Show>
      <div class="list-card">
        <For each={ov()?.engines ?? []}>
          {(e) => {
            const badge = () => BADGE[e.status] ?? { text: e.status, cls: "" };
            return (
              <div class="flex items-center justify-between px-4 py-3">
                <div>
                  <div class="text-sm font-medium">{e.label}</div>
                  <div class="text-xs text-[var(--text-faint)]">{e.id}</div>
                </div>
                <div class="flex items-center gap-3">
                  <div class="text-right">
                    <div class={`text-sm font-medium ${badge().cls}`}>{badge().text}</div>
                    <div class="text-xs text-[var(--text-faint)]">{e.detail}</div>
                  </div>
                  <label
                    class="flex items-center gap-1 text-2xs text-[var(--text-dim)] cursor-pointer"
                    classList={{
                      "opacity-40 pointer-events-none":
                        ov()?.engine === e.id || e.status === "unavailable",
                    }}
                    title="主引擎失败时按列表顺序尝试降级链中的引擎"
                  >
                    <input
                      type="checkbox"
                      checked={inFallback(e.id)}
                      disabled={ov()?.engine === e.id || e.status === "unavailable"}
                      onChange={() => toggleFallback(e.id)}
                    />
                    降级链
                  </label>
                  <button
                    class="pressable px-2.5 py-1 rounded text-xs border border-[var(--border)]"
                    classList={{ "opacity-40": e.status === "unavailable" }}
                    disabled={ov()?.engine === e.id || e.status === "unavailable"}
                    onClick={() => switchEngine(e.id)}
                  >
                    {ov()?.engine === e.id ? "当前引擎" : "设为主引擎"}
                  </button>
                </div>
              </div>
            );
          }}
        </For>
      </div>
      <Show when={ov()}>
        {(o) => (
          <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] px-4 py-3 flex items-center justify-between">
            <div>
              <div class="text-sm">识别语言</div>
              <div class="text-xs text-[var(--text-faint)]">Apple 本地识别与转写的 locale</div>
            </div>
            <select
              class="form-select"
              value={o().locale}
              onChange={(e) => setLocale(e.currentTarget.value)}
            >
              <For each={LOCALES.includes(o().locale) ? LOCALES : [o().locale, ...LOCALES]}>
                {(l) => <option value={l}>{l}</option>}
              </For>
            </select>
          </div>
        )}
      </Show>
      <div class="list-card">
        <For each={keyProviders()}>
          {(e) => (
            <div class="flex items-center gap-3 px-4 py-3">
              <div class="w-24 shrink-0 text-sm">{e.id} 转写 key</div>
              <input
                type="password"
                class="flex-1 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs font-mono"
                placeholder="sk-...（仅存本机 auth.json，0600）"
                value={keys()[e.id] ?? ""}
                onInput={(ev) => setKeys((prev) => ({ ...prev, [e.id]: ev.currentTarget.value }))}
              />
              <button
                class="pressable px-2.5 py-1 rounded text-xs border border-[var(--border)]"
                onClick={() => void saveKey(e.id)}
              >
                保存
              </button>
            </div>
          )}
        </For>
      </div>
    </>
  );
}
