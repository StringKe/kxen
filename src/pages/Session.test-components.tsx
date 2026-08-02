import { Show, type Component } from "solid-js";
import { render } from "solid-js/web";
import type { MsgItem } from "../lib/items";

export function ComposerMock(props: {
  streaming: () => boolean;
  onSend: (text: string, context: never[], images: never[]) => void;
  onStop: () => void;
}) {
  return (
    <div>
      <button onClick={() => props.onSend("首条口信", [], [])}>composer send</button>
      <button onClick={props.onStop}>composer stop</button>
      <Show when={props.streaming()}>
        <span>composer-streaming</span>
      </Show>
    </div>
  );
}

export function UserItemMock(props: { item: MsgItem; onRetry: () => void }) {
  return (
    <div>
      user:{props.item.content}
      <Show when={props.item.sendError}>
        <button onClick={props.onRetry}>发送失败：{props.item.sendError}（点击重发）</button>
      </Show>
    </div>
  );
}

export function AssistantItemMock(props: { item: MsgItem }) {
  return (
    <div>
      assistant:{props.item.content}:model:
      {props.item.model ? `${props.item.model.provider}/${props.item.model.model}` : "none"}
    </div>
  );
}

export const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
export const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export function clickButton(text: string) {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
    item.textContent?.includes(text),
  );
  if (!button) throw new Error(`button not found: ${text}`);
  button.click();
}

export async function mountStreamingSession(Session: Component, setActive: (id: string) => void) {
  setActive("s1");
  const dispose = render(() => <Session />, document.body);
  await flush();
  clickButton("composer send");
  await flush();
  return dispose;
}
