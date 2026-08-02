// MicMenu：语音引擎快捷切换（状态点 + 不可用引擎禁用并明示原因，切换成功才热生效）。
import { createSignal, For, onMount, Show } from "solid-js";
import { ChevronDown } from "lucide-solid";
import { setVoiceEngine, voiceEngines, type VoiceOverview } from "../../lib/voice";
import { createExclusiveDisclosure, onClickOutside } from "../../lib/dismiss";
import { flashErr } from "../../lib/flash";
import { errText } from "../err-text";
import { statusDot } from "../../lib/variants";

const TONE: Record<string, "ok" | "warn" | "err" | "faint"> = {
  ready: "ok",
  needs_auth: "warn",
  unconfigured: "warn",
  unavailable: "err",
};

// 这两态切过去也起不来：禁用而不是点了再失败
const DISABLED = new Set(["unconfigured", "unavailable"]);

export default function MicMenu(props: { onEngine: (id: string) => void }) {
  const { open, setOpen, toggle } = createExclusiveDisclosure();
  const [overview, setOverview] = createSignal<VoiceOverview | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [loadErr, setLoadErr] = createSignal("");
  let root: HTMLDivElement | undefined;
  onClickOutside(
    () => root,
    () => setOpen(false),
  );

  const reload = async () => {
    setLoading(true);
    try {
      setOverview(await voiceEngines());
      setLoadErr("");
    } catch (error) {
      setLoadErr(errText(error));
    } finally {
      setLoading(false);
    }
  };
  onMount(() => void reload());

  const pick = async (id: string) => {
    try {
      await setVoiceEngine(id, overview()?.fallback ?? []);
    } catch (e) {
      // 失败不调 onEngine：前端引擎态必须跟后端实际生效的一致；菜单留着让用户看清状态点
      flashErr(`切换语音引擎失败：${errText(e)}`);
      return;
    }
    await reload();
    props.onEngine(id);
    setOpen(false);
  };

  return (
    <div class="relative" ref={(el) => (root = el)}>
      <button
        class="pressable action-icon"
        title="语音引擎"
        aria-expanded={open()}
        aria-haspopup="menu"
        onClick={toggle}
      >
        <ChevronDown size={12} />
      </button>
      <Show when={open()}>
        <div class="composer-popup absolute bottom-full right-0 mb-1.5 w-52 max-w-[calc(100vw-16px)] rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] overflow-hidden z-20">
          <div class="popup-section">语音引擎</div>
          <Show when={loading() && !overview()}>
            <div class="popup-row text-[var(--text-faint)]">加载中…</div>
          </Show>
          <Show when={loadErr()}>
            <div class="popup-row gap-2 text-[var(--err)]">
              <span class="min-w-0 flex-1 truncate" title={loadErr()}>
                加载语音引擎失败：{loadErr()}
              </span>
              <button
                class="text-[var(--accent-hover)] hover:underline"
                onClick={() => void reload()}
              >
                重试
              </button>
            </div>
          </Show>
          <For each={overview()?.engines ?? []}>
            {(e) => (
              <button
                class="popup-row"
                classList={{ "opacity-50": DISABLED.has(e.status) }}
                disabled={DISABLED.has(e.status)}
                title={DISABLED.has(e.status) ? e.detail : undefined}
                onClick={() => void pick(e.id)}
              >
                <span class={statusDot({ tone: TONE[e.status] ?? "faint" })} />
                <span class="flex-1 text-left truncate" title={e.detail}>
                  {e.label}
                </span>
                <Show when={overview()?.engine === e.id}>
                  <span class="text-2xs text-[var(--accent-hover)]">当前</span>
                </Show>
                <Show when={e.status === "unconfigured"}>
                  <span class="text-2xs text-[var(--text-faint)]">未配置</span>
                </Show>
              </button>
            )}
          </For>
          <Show when={overview() && overview()!.engines.length === 0 && !loadErr()}>
            <div class="popup-row text-[var(--text-faint)]">无可用语音引擎</div>
          </Show>
        </div>
      </Show>
    </div>
  );
}
