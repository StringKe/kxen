import { Show } from "solid-js";

export default function ModelStatusErrors(props: {
  currentError: string;
  globalError: string;
  onRetryCurrent: () => void;
  onRetryGlobal: () => void;
}) {
  return (
    <Show when={props.currentError || props.globalError}>
      <div class="border-b border-[var(--border)] px-2.5 py-1.5 space-y-1 text-2xs text-[var(--err)]">
        <Show when={props.currentError}>
          <div>
            读取生效模型失败：{props.currentError}
            <button
              class="ml-2 text-[var(--accent-hover)] hover:underline"
              onClick={props.onRetryCurrent}
            >
              重试生效模型
            </button>
          </div>
        </Show>
        <Show when={props.globalError}>
          <div>
            读取全局默认失败：{props.globalError}
            <button
              class="ml-2 text-[var(--accent-hover)] hover:underline"
              onClick={props.onRetryGlobal}
            >
              重试全局默认
            </button>
          </div>
        </Show>
      </div>
    </Show>
  );
}
