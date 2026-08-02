// AssistantItem：思考折叠只对 live（流式中的末条）受控——历史条目不绑会话级 streaming，
// 否则任何 run 启停都会强制开/关全列思考块，覆盖用户手动开合状态。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import { closeMenu, menu } from "../lib/context-menu";
import type { MsgItem } from "../lib/items";

// Markdown 走 shiki 高亮链，与本测试断言无关，桩掉保持用例聚焦
vi.mock("./Markdown", () => ({ default: () => null }));

import AssistantItem from "./AssistantItem";

const reasoningItem = (extra: Partial<MsgItem> = {}): MsgItem => ({
  kind: "msg",
  role: "assistant",
  content: "答案",
  reasoning: "思考过程文本",
  messageId: "a1",
  ...extra,
});

function setup(item: MsgItem, live: boolean, streaming = true) {
  return render(
    () => (
      <AssistantItem
        item={item}
        streaming={() => streaming}
        live={() => live}
        onFork={() => {}}
        onRerun={() => {}}
        onContinue={() => {}}
        onRewind={() => {}}
      />
    ),
    document.body,
  );
}

afterEach(() => {
  closeMenu();
  document.body.innerHTML = "";
});

describe("思考折叠受控范围", () => {
  it("live（流式中的末条）：强制展开", () => {
    setup(reasoningItem(), true);
    const details = document.body.querySelector("details");
    expect(details?.open).toBe(true);
  });

  it("历史条目（非 live）：streaming=true 也不得撬开（旧实现绑 streaming 全列强制开合）", () => {
    setup(reasoningItem(), false, true);
    const details = document.body.querySelector("details");
    expect(details?.open).toBe(false);
  });
});

describe("rewind 入口", () => {
  it("无 messageId（流式中的乐观条目）：右键 rewind 禁用", () => {
    setup(reasoningItem({ messageId: undefined }), false);
    const el = document.body.querySelector(".group");
    if (!el) throw new Error("AssistantItem 未渲染");
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 10, clientY: 10 }));
    const rewind = menu()?.items.find((i) => i.label === "回退到此处");
    expect(rewind?.disabled).toBe(true);
  });
});

describe("实际模型署名", () => {
  const stats = {
    ttft_ms: 100,
    duration_ms: 200,
    input_tokens: 10,
    output_tokens: 20,
    tokens_per_sec: 100,
  };

  it("按消息自身持久化的 provider/model 展示", () => {
    setup(
      reasoningItem({
        model: { provider: "anthropic", model: "claude-sonnet-4-6" },
      }),
      false,
    );
    expect(document.body.textContent).toContain("anthropic/claude-sonnet-4-6");
  });

  it("旧消息无 model 时不按当前 picker 伪造署名", () => {
    setup(reasoningItem({ stats }), false);
    expect(document.body.textContent).toContain("in 10 / out 20");
    expect(document.body.textContent).not.toContain("anthropic/");
  });

  it("用量不完整时把已知 tokens 标为下限并显示 UNKNOWN", () => {
    setup(
      reasoningItem({
        stats: { ...stats, unmetered_calls: 1, usage_complete: false },
      }),
      false,
    );
    expect(document.body.textContent).toContain("in ≥10 / out ≥20");
    expect(document.body.textContent).toContain("UNKNOWN");
  });
});
