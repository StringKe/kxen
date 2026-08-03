import { For, Show, type Accessor } from "solid-js";
import { ArrowDown } from "lucide-solid";
import AgentRunCards from "./AgentRunCards";
import EmptyHero from "./EmptyHero";
import SessionItem from "./SessionItem";
import type { Item, MsgItem } from "../lib/items";

export default function SessionTimeline(props: {
  items: Accessor<Item[]>;
  sessionId: Accessor<string>;
  streaming: Accessor<boolean>;
  pinned: Accessor<boolean>;
  loadErr: Accessor<string>;
  timelineLoading: Accessor<boolean>;
  setListRef: (element: HTMLDivElement) => void;
  onScroll: () => void;
  scroll: (force?: boolean) => void;
  retryLoad: () => void;
  onForkId: (messageId: string) => void;
  onEditResend: (index: number, text: string) => Promise<boolean>;
  onRewindId: (messageId: string) => void;
  onRetryItem: (item: MsgItem) => void;
  isRetrying: (item: MsgItem) => boolean;
  onRerun: (index: number) => void;
  onContinue: () => void;
  onRespondApproval: (id: string, allow: boolean) => Promise<void>;
}) {
  return (
    <>
      <div ref={props.setListRef} class="flex-1 overflow-auto px-4 py-5" onScroll={props.onScroll}>
        <div class="w-full space-y-4">
          <For each={props.items()}>
            {(item, index) => (
              <SessionItem
                item={item}
                sessionId={props.sessionId}
                streaming={props.streaming}
                live={() => props.streaming() && index() === props.items().length - 1}
                onForkId={props.onForkId}
                onEditResend={(text) => props.onEditResend(index(), text)}
                onRewindId={props.onRewindId}
                onRetryItem={props.onRetryItem}
                retrying={() => (item.kind === "msg" ? props.isRetrying(item) : false)}
                onRerun={() => props.onRerun(index())}
                onContinue={props.onContinue}
                onImageLoad={() => props.scroll()}
                onRespondApproval={props.onRespondApproval}
              />
            )}
          </For>

          <AgentRunCards />

          <Show when={props.loadErr()}>
            <div class="rounded-lg border border-[var(--err)]/50 bg-[var(--err)]/5 p-6 flex items-center gap-3">
              <span class="text-xs text-[var(--err)]">加载会话失败：{props.loadErr()}</span>
              <button
                class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-xs text-[var(--text-dim)]"
                onClick={props.retryLoad}
              >
                重试
              </button>
            </div>
          </Show>

          <Show when={props.timelineLoading() && props.items().length === 0 && !props.loadErr()}>
            <div class="text-xs text-[var(--text-faint)]">加载会话中…</div>
          </Show>
          <Show when={props.items().length === 0 && !props.loadErr() && !props.timelineLoading()}>
            <EmptyHero />
          </Show>
        </div>
      </div>

      <Show when={!props.pinned()}>
        <button
          class="pressable absolute left-1/2 -translate-x-1/2 bottom-24 z-20 px-2.5 py-1 rounded-full text-2xs border border-[var(--border)] bg-[var(--bg-raised)] text-[var(--text-dim)] composer-popup flex items-center gap-1"
          onClick={() => props.scroll(true)}
        >
          <ArrowDown size={11} /> 回到底部
        </button>
      </Show>
    </>
  );
}
