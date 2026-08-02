// streaming 真源对账：done 不当场清（续跑 run 保持/重臂）；快速终态丢帧由存亡广播收回；
// RPC 失败（running=null）done 路径按终态收回、事件/resync 路径保守保留。
import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  sessionRunning: vi.fn(async (_id: string): Promise<boolean | null> => null),
  streamHandlers: [] as { topics: string[]; cb: (p: unknown) => void }[],
}));

vi.mock("./chat", () => ({ sessionRunning: h.sessionRunning }));
vi.mock("./client", () => ({
  client: {
    stream: (topics: string[]) => ({
      on: (cb: (p: unknown) => void) => {
        h.streamHandlers.push({ topics, cb });
        return () => {};
      },
    }),
  },
}));

import { createStreamingReconcile } from "./streaming-reconcile";

const flush = () => new Promise((r) => setTimeout(r, 0));

function setup(sid = "s1") {
  let active = sid;
  let streaming = "";
  const { reconcile, mountSource } = createStreamingReconcile({
    activeSessionId: () => active,
    streamingSid: () => streaming,
    setStreamingSid: (v) => (streaming = v),
  });
  return {
    reconcile,
    mountSource,
    sid: () => streaming,
    switchTo: (s: string) => (active = s),
    arm: () => (streaming = active),
  };
}

const fireUpdate = () =>
  h.streamHandlers.filter((s) => s.topics.includes("session.update")).forEach((s) => s.cb({}));

beforeEach(() => {
  h.sessionRunning.mockReset().mockResolvedValue(null);
  h.streamHandlers.length = 0;
});

describe("reconcile（done/事件/resync 扳机）", () => {
  it("running=true：保持已臂的 streaming（续跑 run 不丢停止钮）", async () => {
    const c = setup();
    c.arm();
    h.sessionRunning.mockResolvedValue(true);
    c.reconcile("s1", "clear");
    await flush();
    expect(c.sid()).toBe("s1");
  });

  it("running=true：未臂时重臂（中断/interrupt 后续跑恢复进度指示）", async () => {
    const c = setup();
    h.sessionRunning.mockResolvedValue(true);
    c.reconcile("s1", "keep");
    await flush();
    expect(c.sid()).toBe("s1");
  });

  it("running=false：收回 streaming（run 真终态）", async () => {
    const c = setup();
    c.arm();
    h.sessionRunning.mockResolvedValue(false);
    c.reconcile("s1", "keep");
    await flush();
    expect(c.sid()).toBe("");
  });

  it("running=null（RPC 失败）：done 路径按终态收回，事件路径保守保留", async () => {
    const a = setup();
    a.arm();
    a.reconcile("s1", "clear"); // 帧在 = 本 run 已终，不能卡死 streaming
    await flush();
    expect(a.sid()).toBe("");

    const b = setup();
    b.arm();
    b.reconcile("s1", "keep"); // 等下轮事件/resync 再核
    await flush();
    expect(b.sid()).toBe("s1");
  });

  it("核对期间切了会话：晚到响应不得改动新会话的 streaming", async () => {
    const c = setup();
    c.arm();
    h.sessionRunning.mockResolvedValue(false);
    c.reconcile("s1", "clear");
    c.switchTo("s2");
    c.arm();
    await flush();
    expect(c.sid()).toBe("s2");
  });
});

describe("mountSource（session.update 存亡广播）", () => {
  it("快速终态 done 帧丢失：广播驱动真源核对收回 streaming（不卡死）", async () => {
    const c = setup();
    c.mountSource();
    c.arm(); // 发送乐观臂上后 done 帧被 ACL 丢弃，onDone 永不触发
    h.sessionRunning.mockResolvedValue(false);
    fireUpdate();
    await flush();
    expect(h.sessionRunning).toHaveBeenCalledWith("s1");
    expect(c.sid()).toBe("");
  });

  it("广播时 running=true：重臂 streaming（续跑 run 全程有进度指示）", async () => {
    const c = setup();
    c.mountSource();
    h.sessionRunning.mockResolvedValue(true);
    fireUpdate();
    await flush();
    expect(c.sid()).toBe("s1");
  });
});
