// Session 组件层集成：时间线加载恢复等待审批卡 / delta 批量上屏 / Done 对账 /
// resync 自愈 / 发送-排队-停止 / 发送失败重发 / 审批应答。
// lib 层（converge/send/approvals/delta-batch）逻辑各有单测，这里只验 Session 的接线与生命周期。
import { Show } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PendingApproval, RunStats, StoredMessage } from "../lib/chat";
import type { ToolEvent } from "../lib/delta";
import type { MsgItem } from "../lib/items";

const h = vi.hoisted(() => ({
  sessionMessages: vi.fn(async (_id: string): Promise<StoredMessage[]> => []),
  sessionPendingList: vi.fn(async (_id: string): Promise<string[]> => []),
  sessionPendingClear: vi.fn(async (_id: string): Promise<void> => {}),
  approvalPending: vi.fn(async (_id: string): Promise<PendingApproval[]> => []),
  approvalRespond: vi.fn(async (_id: string, _allow: boolean) => ({ resolved: true })),
  statusline: vi.fn(async () => null),
  sessionRunning: vi.fn(async (_id: string): Promise<boolean | null> => null),
  sessionAbort: vi.fn(async (_id: string) => true),
  sendMessage: vi.fn(async (_sid: string, _text: string, _c: unknown[], _i: unknown[]) => ({
    queued: false,
  })),
  // onLlmDelta 的回调按次捕获：测试手动驱动 delta/done/resync 帧
  delta: {} as {
    onText?: (text: string) => void;
    onReasoning?: (text: string) => void;
    onDone?: (stats?: RunStats, error?: string) => void;
    onEvent?: (event: ToolEvent) => void;
    onResync?: () => void;
  },
  onLlmDelta: vi.fn(
    (
      _active: () => string,
      onText: (text: string) => void,
      onReasoning: (text: string) => void,
      onDone: (stats?: RunStats, error?: string) => void,
      onEvent?: (event: ToolEvent) => void,
      onResync?: () => void,
    ) => {
      h.delta = { onText, onReasoning, onDone, onEvent, onResync };
      return () => {};
    },
  ),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    sessionMessages: h.sessionMessages,
    sessionPendingList: h.sessionPendingList,
    sessionPendingClear: h.sessionPendingClear,
    approvalPending: h.approvalPending,
    approvalRespond: h.approvalRespond,
    statusline: h.statusline,
    sessionRunning: h.sessionRunning,
    sessionAbort: h.sessionAbort,
    sendMessage: h.sendMessage,
    onLlmDelta: h.onLlmDelta,
  };
});

// Composer 桩出发送/停止入口；UserItem/AssistantItem 桩出动作按钮（各自有组件单测）
vi.mock("../components/composer/TextComposer", () => ({
  default: (props: {
    streaming: () => boolean;
    onSend: (t: string, c: never[], i: never[]) => void;
    onStop: () => void;
  }) => (
    <div>
      <button onClick={() => props.onSend("首条口信", [], [])}>composer send</button>
      <button onClick={props.onStop}>composer stop</button>
      <Show when={props.streaming()}>
        <span>composer-streaming</span>
      </Show>
    </div>
  ),
}));

vi.mock("../components/UserItem", () => ({
  default: (props: { item: MsgItem; onRetry: () => void }) => (
    <div>
      user:{props.item.content}
      <Show when={props.item.sendError}>
        <button onClick={props.onRetry}>发送失败：{props.item.sendError}（点击重发）</button>
      </Show>
    </div>
  ),
}));

vi.mock("../components/AssistantItem", () => ({
  default: (props: { item: MsgItem }) => <div>assistant:{props.item.content}</div>,
}));

import Session from "./Session";
import { setActiveSessionId } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const clickButton = (text: string) => {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
    item.textContent?.includes(text),
  );
  if (!button) throw new Error(`button not found: ${text}`);
  button.click();
};

/** 进入 streaming：活跃会话 s1 就绪后点发送（sendMessage 默认 queued:false 首发成功）。 */
async function mountStreaming() {
  setActiveSessionId("s1");
  const dispose = render(() => <Session />, document.body);
  await flush();
  clickButton("composer send");
  await flush();
  return dispose;
}

afterEach(() => {
  document.body.innerHTML = "";
  setActiveSessionId("");
  h.delta = {};
  for (const fn of Object.values(h)) if (vi.isMockFunction(fn)) fn.mockClear();
  h.sessionMessages.mockImplementation(async () => []);
  h.approvalPending.mockImplementation(async () => []);
  h.approvalRespond.mockImplementation(async () => ({ resolved: true }));
  h.sessionRunning.mockImplementation(async () => null);
  h.sendMessage.mockImplementation(async () => ({ queued: false }));
});

describe("Session 时间线加载", () => {
  it("approval.pending 快照恢复为等待审批卡", async () => {
    h.approvalPending.mockImplementation(async () => [
      { id: "ap1", command: "rm -rf /tmp/x", reason: "危险命令" } as PendingApproval,
    ]);
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    expect(h.approvalPending).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).toContain("审批请求：危险命令");
    expect(document.body.textContent).toContain("rm -rf /tmp/x");
    clickButton("允许"); // 等待卡可操作（未决）
    await flush();
    expect(h.approvalRespond).toHaveBeenCalledWith("ap1", true);
    expect(document.body.textContent).toContain("已允许");
    dispose();
  });

  it("迟到的应答（服务端已了结）置失效而非冒充用户决定", async () => {
    h.approvalPending.mockImplementation(async () => [
      { id: "ap2", command: "deploy", reason: "上线确认" } as PendingApproval,
    ]);
    h.approvalRespond.mockImplementation(async () => ({ resolved: false }));
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    clickButton("拒绝");
    await flush();
    expect(h.approvalRespond).toHaveBeenCalledWith("ap2", false);
    expect(document.body.textContent).toContain("已失效");
    expect(document.body.textContent).not.toContain("已拒绝");
    dispose();
  });
});

describe("Session 流式与对账", () => {
  it("delta 50ms 窗口合并上屏，orb 切生成中", async () => {
    const dispose = await mountStreaming();
    expect(document.body.textContent).toContain("composer-streaming");
    h.delta.onText?.("增量甲");
    h.delta.onText?.("增量乙");
    expect(document.body.textContent).not.toContain("增量甲"); // 合并窗口内未上屏
    await sleep(70);
    expect(document.body.textContent).toContain("增量甲增量乙"); // 同气泡合并（stub 每气泡一条 assistant: 前缀）
    expect(document.body.textContent).toContain("生成中");
    dispose();
  });

  it("Done：残余 delta 先上屏，再以存储快照对账为最终权威", async () => {
    const snapshot: StoredMessage[] = [
      {
        id: "a9",
        session_id: "s1",
        role: "assistant",
        parts: [{ type: "text", text: "落盘终稿" }],
        created_at: 2,
      },
    ];
    let calls = 0;
    h.sessionMessages.mockImplementation(async () => {
      calls += 1;
      return calls === 1 ? [] : snapshot;
    });
    const dispose = await mountStreaming();
    expect(calls).toBe(1); // 首次时间线加载
    h.delta.onText?.("临时增量");
    h.delta.onDone?.();
    await flush();
    expect(calls).toBe(2); // Done 触发 converge 重拉快照
    expect(h.sessionPendingList).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).toContain("落盘终稿");
    expect(document.body.textContent).not.toContain("临时增量"); // 快照权威替换流式草稿
    expect(document.body.textContent).not.toContain("composer-streaming");
    dispose();
  });

  it("resync：只对账，run 仍在跑（running=true）保留 streaming", async () => {
    h.sessionRunning.mockImplementation(async () => true);
    const dispose = await mountStreaming();
    h.delta.onResync?.();
    await flush();
    expect(h.sessionRunning).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).toContain("composer-streaming"); // 停止按钮不丢
    dispose();
  });

  it("resync：done 在断线窗口丢失（running=false）按真源收回 streaming", async () => {
    h.sessionRunning.mockImplementation(async () => false);
    const dispose = await mountStreaming();
    h.delta.onResync?.();
    await flush();
    expect(h.sessionRunning).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).not.toContain("composer-streaming");
    dispose();
  });
});

describe("Session 发送链路", () => {
  it("乐观上屏 -> queued 转排队预览 -> 停止清队列并 abort", async () => {
    h.sendMessage.mockImplementation(async () => ({ queued: true }));
    const dispose = await mountStreaming();
    expect(h.sendMessage).toHaveBeenCalledWith("s1", "首条口信", [], []);
    expect(document.body.textContent).toContain("user:首条口信"); // 乐观气泡
    expect(document.body.textContent).toContain("排队中 1 条"); // queued 反馈
    clickButton("composer stop");
    await flush();
    expect(h.sessionAbort).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).not.toContain("排队中"); // 清队列是用户本意
    dispose();
  });

  it("PendingQueue 清空：sessionPendingClear 后按真源重载", async () => {
    h.sendMessage.mockImplementation(async () => ({ queued: true }));
    const dispose = await mountStreaming();
    expect(document.body.textContent).toContain("排队中 1 条");
    clickButton("清空");
    await flush();
    expect(h.sessionPendingClear).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).not.toContain("排队中");
    dispose();
  });

  it("发送失败挂失败态，点击重发撤下原气泡走完整发送链", async () => {
    h.sendMessage.mockRejectedValueOnce(new Error("boom"));
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    clickButton("composer send");
    await flush();
    expect(document.body.textContent).toContain("发送失败");
    expect(document.body.textContent).not.toContain("composer-streaming"); // 首发失败收回 streaming
    clickButton("点击重发");
    await flush();
    expect(h.sendMessage).toHaveBeenCalledTimes(2);
    expect(h.sendMessage).toHaveBeenLastCalledWith("s1", "首条口信", [], []);
    expect(document.body.textContent).not.toContain("发送失败");
    expect(document.body.textContent).toContain("composer-streaming"); // 重发成功恢复
    dispose();
  });
});
