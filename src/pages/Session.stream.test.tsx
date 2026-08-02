import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ModelIdentity, PendingApproval, RunStats, StoredMessage } from "../lib/chat";
import type { ToolEvent } from "../lib/delta";
import { clickButton, flush, mountStreamingSession, sleep } from "./Session.test-components";

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
  sessionUpdateHandlers: [] as Array<(p: unknown) => void>,
  delta: {} as {
    onText?: (text: string) => void;
    onReasoning?: (text: string) => void;
    onDone?: (stats?: RunStats, error?: string) => void;
    onEvent?: ((event: ToolEvent) => void) | undefined;
    onResync?: (() => void) | undefined;
    onModel?: ((model: ModelIdentity) => void) | undefined;
  },
  onLlmDelta: vi.fn(
    (
      _active: () => string,
      onText: (text: string) => void,
      onReasoning: (text: string) => void,
      onDone: (stats?: RunStats, error?: string) => void,
      onEvent?: (event: ToolEvent) => void,
      onResync?: () => void,
      onModel?: (model: ModelIdentity) => void,
    ) => {
      h.delta = { onText, onReasoning, onDone, onEvent, onResync, onModel };
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

vi.mock("../lib/client", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/client")>();
  return {
    ...orig,
    client: {
      ...orig.client,
      stream: (topics: string | string[]) => ({
        on: (cb: (p: unknown) => void) => {
          if (topics === "session.update") h.sessionUpdateHandlers.push(cb);
          return () => {};
        },
      }),
    },
  };
});

vi.mock("../components/composer/TextComposer", async () => ({
  default: (await import("./Session.test-components")).ComposerMock,
}));

vi.mock("../components/UserItem", async () => ({
  default: (await import("./Session.test-components")).UserItemMock,
}));
vi.mock("../components/AssistantItem", async () => ({
  default: (await import("./Session.test-components")).AssistantItemMock,
}));

import Session from "./Session";
import { flash } from "../lib/flash";
import { setActiveSessionId } from "../lib/state";

/** 进入 streaming：活跃会话 s1 就绪后点发送（sendMessage 默认 queued:false 首发成功）。 */
const mountStreaming = () => mountStreamingSession(Session, setActiveSessionId);

afterEach(() => {
  document.body.innerHTML = "";
  setActiveSessionId("");
  h.delta = {};
  h.sessionUpdateHandlers.length = 0;
  for (const fn of Object.values(h)) if (vi.isMockFunction(fn)) fn.mockClear();
  h.sessionMessages.mockImplementation(async () => []);
  h.approvalPending.mockImplementation(async () => []);
  h.approvalRespond.mockImplementation(async () => ({ resolved: true }));
  h.sessionRunning.mockImplementation(async () => null);
  h.sessionAbort.mockImplementation(async () => true);
  h.sendMessage.mockImplementation(async () => ({ queued: false }));
  for (const message of flash.msgs()) flash.dismiss(message.id);
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
  it("statusline 尚未返回时已同步注册 delta 订阅", () => {
    h.statusline.mockImplementationOnce(() => new Promise(() => {}));
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    expect(h.onLlmDelta).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("delta 50ms 窗口合并上屏，orb 切生成中", async () => {
    const dispose = await mountStreaming();
    expect(document.body.textContent).toContain("composer-streaming");
    h.delta.onModel?.({ provider: "anthropic", model: "claude-sonnet-4-6" });
    h.delta.onText?.("增量甲");
    h.delta.onText?.("增量乙");
    expect(document.body.textContent).not.toContain("增量甲"); // 合并窗口内未上屏
    await sleep(70);
    expect(document.body.textContent).toContain("增量甲增量乙"); // 同气泡合并（stub 每气泡一条 assistant: 前缀）
    expect(document.body.textContent).toContain("anthropic/claude-sonnet-4-6");
    expect(document.body.textContent).toContain("生成中");
    dispose();
  });

  it("切换会话丢弃旧会话的待刷新 delta，新会话只使用自己的实际模型", async () => {
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();

    h.delta.onModel?.({ provider: "xai", model: "grok-4" });
    h.delta.onText?.("旧会话残片");
    setActiveSessionId("s2");
    await flush();
    h.delta.onModel?.({ provider: "anthropic", model: "claude-sonnet-4-6" });
    h.delta.onText?.("新会话正文");

    await sleep(70);
    expect(document.body.textContent).toContain("新会话正文");
    expect(document.body.textContent).toContain("anthropic/claude-sonnet-4-6");
    expect(document.body.textContent).not.toContain("旧会话残片");
    expect(document.body.textContent).not.toContain("xai/grok-4");
    dispose();
  });

  it("同会话实际模型变更时不把前一模型的待刷新 delta 重标为新模型", async () => {
    const dispose = await mountStreaming();
    h.delta.onModel?.({ provider: "xai", model: "grok-4" });
    h.delta.onText?.("模型甲片段");
    h.delta.onModel?.({ provider: "anthropic", model: "claude-sonnet-4-6" });
    h.delta.onText?.("模型乙片段");

    await sleep(70);
    expect(document.body.textContent).toContain("模型甲片段:model:xai/grok-4");
    expect(document.body.textContent).toContain("模型乙片段:model:anthropic/claude-sonnet-4-6");
    dispose();
  });

  it("工具事件上屏前先刷新更早的文本 delta，不颠倒时间线顺序", async () => {
    const dispose = await mountStreaming();
    h.delta.onText?.("先到的文本");
    h.delta.onEvent?.({ kind: "tool_call", name: "read", summary: "后到的工具" });

    const timeline = document.body.textContent ?? "";
    expect(timeline.indexOf("先到的文本")).toBeGreaterThanOrEqual(0);
    expect(timeline.indexOf("先到的文本")).toBeLessThan(timeline.indexOf("后到的工具"));
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

  it("Done 后续跑 run（running=true）：streaming 保持，进度指示/停止钮不丢", async () => {
    h.sessionRunning.mockImplementation(async () => true);
    const dispose = await mountStreaming();
    h.delta.onDone?.();
    await flush();
    expect(h.sessionRunning).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).toContain("composer-streaming");
    dispose();
  });

  it("快速终态 done 帧被 ACL 丢弃：session.update 存亡广播按真源收回（不卡死）", async () => {
    h.sessionRunning.mockImplementation(async () => false);
    const dispose = await mountStreaming();
    expect(document.body.textContent).toContain("composer-streaming");
    // onDone 永不触发，只有 RunGuard 存亡广播兜底
    h.sessionUpdateHandlers.forEach((cb) => cb({}));
    await flush();
    expect(document.body.textContent).not.toContain("composer-streaming");
    dispose();
  });

  it("session.update 存亡广播 running=true：未臂时重臂 streaming（续跑恢复进度指示）", async () => {
    h.sessionRunning.mockImplementation(async () => true);
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    expect(document.body.textContent).not.toContain("composer-streaming");
    h.sessionUpdateHandlers.forEach((cb) => cb({}));
    await flush();
    expect(document.body.textContent).toContain("composer-streaming");
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

  it("abort RPC 失败保留队列并显示错误，不伪造已停止状态", async () => {
    h.sendMessage.mockImplementation(async () => ({ queued: true }));
    h.sessionAbort.mockRejectedValueOnce(new Error("backend unavailable"));
    const dispose = await mountStreaming();
    expect(document.body.textContent).toContain("排队中 1 条");
    clickButton("composer stop");
    await flush();
    expect(document.body.textContent).toContain("排队中 1 条");
    expect(
      flash.msgs().some((message) => message.kind === "err" && message.text.includes("停止失败")),
    ).toBe(true);
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
