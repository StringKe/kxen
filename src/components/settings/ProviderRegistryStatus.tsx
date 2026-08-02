import { Show } from "solid-js";

export default function ProviderRegistryStatus(props: {
  loaded: boolean;
  error: string;
  stale: boolean;
  onRetry: () => void;
}) {
  return (
    <>
      <Show when={!props.loaded}>
        <div class="text-xs text-[var(--text-faint)]">加载 provider 清单…</div>
      </Show>
      <Show when={props.error}>
        <div class="flex items-center gap-2 text-xs text-[var(--err)]">
          <span>
            {props.stale ? "刷新 provider 清单失败，正在显示上次结果" : "加载 provider 清单失败"}：
            {props.error}
          </span>
          <button
            class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-[var(--text-dim)]"
            onClick={props.onRetry}
          >
            重试
          </button>
        </div>
      </Show>
    </>
  );
}
