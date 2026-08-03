// Session 路由卸载重挂载：prevSid 是组件实例变量，重挂载后重置。
// null 哨兵首跑强制重载（修：重挂载误判 fromDraft 跳过重载 = 时间线空白）；
// 草稿->激活首发仍跳过重载（空载会抹掉乐观上屏 = 首行消失）。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { StoredMessage } from "../lib/chat";

const h = vi.hoisted(() => ({
  sessionMessages: vi.fn(
    async (_id: string): Promise<StoredMessage[]> => [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        parts: [{ type: "text", text: "历史消息一" }],
        created_at: 1,
      },
    ],
  ),
  sessionPendingList: vi.fn(async (_id: string): Promise<string[]> => []),
  // 时间线加载链并行拉等待中审批：不桩则打到真 RPC（无后端挂起，Promise.all 永不回 = 时间线空白）
  approvalPending: vi.fn(
    async (_id: string): Promise<import("../lib/chat").PendingApproval[]> => [],
  ),
  statusline: vi.fn(async () => null),
  onLlmDelta: vi.fn(() => () => {}),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  // 全量 mock 会断 state.ts 的 sessionCreate/sessionList 绑定：铺开真实模块，只桩时间线相关的 4 个
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    sessionMessages: h.sessionMessages,
    sessionPendingList: h.sessionPendingList,
    approvalPending: h.approvalPending,
    statusline: h.statusline,
    onLlmDelta: h.onLlmDelta,
  };
});

// Composer（语音/命令 RPC）与 AssistantItem（shiki）与本测试无关，桩掉保持用例聚焦
vi.mock("../components/composer/TextComposer", () => ({ default: () => null }));
vi.mock("../components/StorageRecoveryPanel", () => ({ default: () => null }));
vi.mock("../components/AssistantItem", () => ({ default: () => null }));

import Session from "./Session";
import { setActiveSessionId } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  document.body.innerHTML = "";
  setActiveSessionId("");
  h.sessionMessages.mockClear();
  h.sessionPendingList.mockClear();
});

describe("Session 路由卸载重挂载", () => {
  it("重挂载后时间线从存储强制重载（不残留 EmptyHero）", async () => {
    setActiveSessionId("s1");
    const dispose1 = render(() => <Session />, document.body);
    await flush();
    expect(document.body.textContent).toContain("历史消息一");
    expect(h.sessionMessages).toHaveBeenCalledTimes(1);
    dispose1();

    // 路由切走再切回：组件实例全新（prevSid 重置），state 里的活跃会话不变
    const dispose2 = render(() => <Session />, document.body);
    await flush();
    expect(h.sessionMessages).toHaveBeenCalledTimes(2);
    expect(document.body.textContent).toContain("历史消息一");
    dispose2();
  });

  it("草稿->激活首发仍跳过重载（乐观上屏不被空载抹掉）", async () => {
    const dispose = render(() => <Session />, document.body); // 草稿态 ""
    await flush();
    setActiveSessionId("s2"); // 首发落库后激活
    await flush();
    expect(h.sessionMessages).not.toHaveBeenCalled();
    dispose();
  });

  it("已落库会话切换时立即撤下旧时间线，慢响应期间不可操作旧消息", async () => {
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    expect(document.body.textContent).toContain("历史消息一");

    let resolveSecond!: (messages: StoredMessage[]) => void;
    h.sessionMessages.mockImplementationOnce(
      () =>
        new Promise<StoredMessage[]>((resolve) => {
          resolveSecond = resolve;
        }),
    );
    setActiveSessionId("s2");
    await flush();

    expect(document.body.textContent).not.toContain("历史消息一");
    expect(document.body.textContent).toContain("加载会话中");

    resolveSecond([
      {
        id: "m2",
        session_id: "s2",
        role: "user",
        parts: [{ type: "text", text: "会话二消息" }],
        created_at: 2,
      },
    ]);
    await flush();
    expect(document.body.textContent).toContain("会话二消息");
    expect(document.body.textContent).not.toContain("历史消息一");
    dispose();
  });
});

describe("Session 首载错误态", () => {
  it("时间线加载失败出错误条 + 重试（不与 EmptyHero 同形），重试成功恢复", async () => {
    setActiveSessionId("s1");
    h.sessionMessages.mockRejectedValueOnce(new Error("backend down"));
    const dispose = render(() => <Session />, document.body);
    await flush();
    expect(document.body.textContent).toContain("加载会话失败");
    expect(document.body.textContent).toContain("backend down"); // 原因上屏
    expect(document.body.textContent).not.toContain("历史消息一");

    const retry = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (b) => b.textContent === "重试",
    );
    expect(retry).toBeTruthy();
    retry?.click();
    await flush();
    expect(document.body.textContent).toContain("历史消息一");
    expect(document.body.textContent).not.toContain("加载会话失败");
    dispose();
  });
});
