import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Item } from "../lib/items";

vi.mock("./AssistantItem", () => ({
  default: (props: {
    onFork: () => void;
    onRerun: () => void;
    onContinue: () => void;
    onRewind: () => void;
  }) => (
    <div>
      assistant
      <button onClick={props.onFork}>assistant fork</button>
      <button onClick={props.onRerun}>rerun</button>
      <button onClick={props.onContinue}>continue</button>
      <button onClick={props.onRewind}>assistant rewind</button>
    </div>
  ),
}));

vi.mock("./UserItem", () => ({
  default: (props: {
    onFork: () => void;
    onEditResend: (text: string) => void;
    onRewind: () => void;
    onRetry: () => void;
    onImageLoad: () => void;
  }) => (
    <div>
      user
      <button onClick={props.onFork}>user fork</button>
      <button onClick={() => props.onEditResend("edited")}>edit</button>
      <button onClick={props.onRewind}>user rewind</button>
      <button onClick={props.onRetry}>retry</button>
      <button onClick={props.onImageLoad}>image</button>
    </div>
  ),
}));

import SessionItem from "./SessionItem";

const clickButton = (text: string) => {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
    item.textContent?.includes(text),
  );
  if (!button) throw new Error(`button not found: ${text}`);
  button.click();
};

afterEach(() => {
  document.body.innerHTML = "";
});

describe("SessionItem 分派", () => {
  it("分派 tool、approval、phase、compacted、user 和 assistant", () => {
    const callbacks = {
      onForkId: vi.fn(),
      onEditResend: vi.fn(),
      onRewindId: vi.fn(),
      onRetryItem: vi.fn(),
      onRerun: vi.fn(),
      onContinue: vi.fn(),
      onImageLoad: vi.fn(),
      onRespondApproval: vi.fn(async () => {}),
    };
    const common = {
      streaming: () => false,
      live: () => true,
      ...callbacks,
    };
    const items: Item[] = [
      { kind: "tool", name: "shell", call: "run", result: "ok" },
      { kind: "approval", approvalId: "a1", command: "run", reason: "reason" },
      { kind: "phase", name: "build", index: 1, total: 2, workflow: "release" },
      { kind: "phase", name: "plain" },
      { kind: "compacted", summary: "summary" },
      { kind: "msg", role: "user", content: "user", messageId: "u1" },
      { kind: "msg", role: "assistant", content: "assistant", messageId: "a1" },
    ];
    for (const item of items) {
      document.body.innerHTML = "";
      const dispose = render(() => <SessionItem item={item} {...common} />, document.body);
      expect(document.body.textContent?.length).toBeGreaterThan(0);
      if (item.kind === "msg" && item.role === "user") {
        clickButton("user fork");
        clickButton("edit");
        clickButton("user rewind");
        clickButton("retry");
        clickButton("image");
      }
      if (item.kind === "msg" && item.role === "assistant") {
        clickButton("assistant fork");
        clickButton("rerun");
        clickButton("continue");
        clickButton("assistant rewind");
      }
      dispose();
    }
    expect(callbacks.onForkId).toHaveBeenCalledWith("u1");
    expect(callbacks.onForkId).toHaveBeenCalledWith("a1");
    expect(callbacks.onEditResend).toHaveBeenCalledWith("edited");
    expect(callbacks.onRewindId).toHaveBeenCalledWith("u1");
    expect(callbacks.onRewindId).toHaveBeenCalledWith("a1");
    expect(callbacks.onRetryItem).toHaveBeenCalled();
    expect(callbacks.onRerun).toHaveBeenCalled();
    expect(callbacks.onContinue).toHaveBeenCalled();
    expect(callbacks.onImageLoad).toHaveBeenCalled();
  });
});
