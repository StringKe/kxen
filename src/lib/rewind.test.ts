// rewind 确认流：dirty 门禁的「拒绝 -> 确认 -> 带 confirm 重发」序列，其余拒绝不重试。
// fixture 与 src-tauri/src/ws/session_ops.rs 的 RewindBlock 序列化对齐（code 驱动，漂移即测试红）。
import { afterEach, describe, expect, it, vi } from "vitest";
import { createRoot, createSignal } from "solid-js";
import {
  classifyRewindError,
  createRewindFlow,
  createSessionRewind,
  parseRewindError,
  rewindErrorText,
  rewindPendingInfo,
  type RewindPendingInfo,
} from "./rewind";

const block = (code: string, extra: Record<string, unknown> = {}) =>
  new Error(JSON.stringify({ code, message: `mock ${code} 人话`, ...extra }));
const DIRTY = block("dirty", {
  dirty_count: 2,
  target: { id: "m-9", role: "user", preview: "帮我改一下" },
});
const ACTIVE_RUN = block("active_run");
const NOT_IN_SESSION = block("not_in_session");
const CHECKPOINT_MISSING = block("checkpoint_missing");

type Call = { sid: string; mid: string; confirm: boolean };

function harness(rejectOnce?: Error, rejectAlways?: Error) {
  const calls: Call[] = [];
  const done: string[] = [];
  const errors: string[] = [];
  const pendings: (string | null)[] = [];
  const infos: (RewindPendingInfo | null)[] = [];
  const flow = createRewindFlow({
    sessionId: () => "s1",
    call: (sid, mid, confirm) => {
      calls.push({ sid, mid, confirm });
      const err = rejectAlways ?? (calls.length === 1 ? rejectOnce : undefined);
      return err ? Promise.reject(err) : Promise.resolve();
    },
    onPendingChange: (id) => pendings.push(id),
    onPendingInfo: (info) => infos.push(info),
    onDone: () => done.push("done"),
    onError: (t) => errors.push(t),
  });
  return { flow, calls, done, errors, pendings, infos };
}

describe("createRewindFlow", () => {
  it("dirty 拒绝后进待确认并带上下文，用户确认带 confirm=true 重发同一消息", async () => {
    const h = harness(DIRTY);
    await h.flow.request("m-9");
    // 第一次不带 confirm，被拒后挂起等待用户决定，不算完成也不算错误
    expect(h.calls).toEqual([{ sid: "s1", mid: "m-9", confirm: false }]);
    expect(h.flow.pending()).toBe("m-9");
    expect(h.done).toEqual([]);
    expect(h.errors).toEqual([]);
    // 确认框上下文：dirty 文件数 + 目标消息摘要来自 RewindBlock 载荷
    expect(h.infos).toEqual([
      { messageId: "m-9", dirtyCount: 2, targetRole: "user", targetPreview: "帮我改一下" },
    ]);

    await h.flow.confirm();
    expect(h.calls).toEqual([
      { sid: "s1", mid: "m-9", confirm: false },
      { sid: "s1", mid: "m-9", confirm: true },
    ]);
    expect(h.flow.pending()).toBeNull();
    expect(h.infos).toEqual([
      { messageId: "m-9", dirtyCount: 2, targetRole: "user", targetPreview: "帮我改一下" },
      null,
    ]);
    expect(h.done).toEqual(["done"]);
    expect(h.errors).toEqual([]);
  });

  it("active run 拒绝：直接报错，不重试、不进待确认、confirm 空转", async () => {
    const h = harness(ACTIVE_RUN, ACTIVE_RUN);
    await h.flow.request("m-1");
    expect(h.calls).toEqual([{ sid: "s1", mid: "m-1", confirm: false }]);
    expect(h.flow.pending()).toBeNull();
    expect(h.done).toEqual([]);
    expect(h.errors).toEqual([
      "工作区有任务正在运行，回退会覆盖它正在写的文件，请先停止或等它完成",
    ]);

    // 无待确认项时 confirm 不得触发任何重发
    await h.flow.confirm();
    expect(h.calls).toHaveLength(1);
  });

  it("跨 session 拒绝：不重试，文案指向消息归属", async () => {
    const h = harness(NOT_IN_SESSION, NOT_IN_SESSION);
    await h.flow.request("m-1");
    expect(h.calls).toHaveLength(1);
    expect(h.flow.pending()).toBeNull();
    expect(h.errors).toEqual(["这条消息不在当前会话中，无法回退到此处"]);
  });

  it("取消待确认：清空 pending 与上下文且不再重发", async () => {
    const h = harness(DIRTY);
    await h.flow.request("m-9");
    expect(h.flow.pending()).toBe("m-9");
    h.flow.cancel();
    expect(h.flow.pending()).toBeNull();
    expect(h.infos.at(-1)).toBeNull();
    await h.flow.confirm();
    expect(h.calls).toHaveLength(1);
    expect(h.done).toEqual([]);
  });

  it("confirm 重发仍被拒（确认期间起了 run）：走错误提示，不再挂确认", async () => {
    const calls: Call[] = [];
    const errors: string[] = [];
    const flow = createRewindFlow({
      sessionId: () => "s1",
      call: (sid, mid, confirm) => {
        calls.push({ sid, mid, confirm });
        return Promise.reject(confirm ? ACTIVE_RUN : DIRTY);
      },
      onError: (t) => errors.push(t),
    });
    await flow.request("m-9");
    expect(flow.pending()).toBe("m-9");
    await flow.confirm();
    expect(flow.pending()).toBeNull();
    expect(calls.map((c) => c.confirm)).toEqual([false, true]);
    expect(errors).toEqual(["工作区有任务正在运行，回退会覆盖它正在写的文件，请先停止或等它完成"]);
  });
});

describe("classifyRewindError / rewindErrorText", () => {
  it("按 code 归类三种门禁，文案子串不再参与归类", () => {
    expect(classifyRewindError(DIRTY)).toBe("dirty");
    expect(classifyRewindError(ACTIVE_RUN)).toBe("active_run");
    expect(classifyRewindError(NOT_IN_SESSION)).toBe("not_in_session");
    expect(classifyRewindError(CHECKPOINT_MISSING)).toBe("checkpoint_missing");
    // 旧的手写英文 fixture 形态（纯文案）一律 unknown：归类只看结构化 code
    expect(
      classifyRewindError(new Error("worktree has uncheckpointed changes, pass confirm=true")),
    ).toBe("unknown");
    expect(classifyRewindError(new Error("workspace has an active run, rewind refused"))).toBe(
      "unknown",
    );
    expect(classifyRewindError(new Error("rpc timeout: session.rewind"))).toBe("unknown");
  });

  it("结构化但 code 未识别：归类 unknown，兜底文案取载荷里的人话", () => {
    const err = block("rate_limited");
    expect(classifyRewindError(err)).toBe("unknown");
    expect(rewindErrorText(err)).toBe("回退失败：mock rate_limited 人话");
  });

  it("parseRewindError：非结构化错误返回 null，结构化载荷字段可读", () => {
    expect(parseRewindError(new Error("boom"))).toBeNull();
    expect(parseRewindError(DIRTY)?.dirty_count).toBe(2);
    expect(parseRewindError(DIRTY)?.target?.preview).toBe("帮我改一下");
  });

  it("未识别错误保留原始信息便于排查", () => {
    expect(rewindErrorText(new Error("boom"))).toBe("回退失败：boom");
  });

  it("checkpoint_missing（barrier commit 失败只 warn，rewind 才暴露）：归类人话，不再裸报英文", () => {
    expect(rewindErrorText(CHECKPOINT_MISSING)).toBe(
      "这条消息的代码检查点没有保存成功，无法回退到此处",
    );
  });
});

describe("createRewindFlow 防抖", () => {
  it("request 在飞期间重复触发不再发 RPC，busy 暴露给确认键禁用", async () => {
    let release!: () => void;
    const calls: Call[] = [];
    const flow = createRewindFlow({
      sessionId: () => "s1",
      call: (sid, mid, confirm) => {
        calls.push({ sid, mid, confirm });
        return new Promise<void>((r) => {
          release = r;
        });
      },
    });
    expect(flow.busy()).toBe(false);
    const p1 = flow.request("m-1");
    expect(flow.busy()).toBe(true);
    await flow.request("m-2"); // 在飞被拒：立即返回，不发第二个 RPC
    expect(calls).toHaveLength(1);
    release();
    await p1;
    expect(flow.busy()).toBe(false);
  });

  it("confirm 在飞时重复确认只发一次带 confirm 的请求", async () => {
    let releaseConfirm!: () => void;
    const calls: Call[] = [];
    const flow = createRewindFlow({
      sessionId: () => "s1",
      call: (sid, mid, confirm) => {
        calls.push({ sid, mid, confirm });
        return confirm
          ? new Promise<void>((r) => {
              releaseConfirm = r;
            })
          : Promise.reject(DIRTY);
      },
    });
    await flow.request("m-9");
    expect(flow.pending()).toBe("m-9");
    const p1 = flow.confirm();
    await flow.confirm(); // 在飞被拒：不得重复发 confirm=true
    expect(calls.map((c) => c.confirm)).toEqual([false, true]);
    releaseConfirm();
    await p1;
    expect(flow.pending()).toBeNull();
  });
});

describe("createSessionRewind 错误尾注", () => {
  afterEach(() => vi.useRealTimers());

  function noteHarness() {
    vi.useFakeTimers();
    // createSessionRewind 内含 createEffect（切会话清 pending）：挂 root 消除未处置警告
    return createRoot((dispose) => {
      const r = createSessionRewind({
        sessionId: () => "s1",
        onDone: () => {},
        call: () => Promise.reject(ACTIVE_RUN),
      });
      return {
        note: r.note,
        dismiss: r.dismissNote,
        fire: (mid: string) => r.flow.request(mid),
        dispose,
      };
    });
  }

  it("报错上尾注，4s 自动消失", async () => {
    const h = noteHarness();
    await h.fire("m-1");
    expect(h.note()).toContain("正在运行");
    vi.advanceTimersByTime(3999);
    expect(h.note()).not.toBe("");
    vi.advanceTimersByTime(1);
    expect(h.note()).toBe("");
    h.dispose();
  });

  it("点击关闭立即消，且旧计时器不再清掉后续文案", async () => {
    const h = noteHarness();
    await h.fire("m-1");
    expect(h.note()).not.toBe("");
    h.dismiss();
    expect(h.note()).toBe("");
    // 再次报错：新文案不被第一次的计时器抢清
    await h.fire("m-2");
    vi.advanceTimersByTime(3999);
    expect(h.note()).not.toBe("");
    vi.advanceTimersByTime(1);
    expect(h.note()).toBe("");
    h.dispose();
  });
});

describe("createSessionRewind 确认框上下文通道", () => {
  afterEach(() => vi.useRealTimers());

  it("dirty 挂起时 RewindConfirm 通道带上下文，确认后清空", async () => {
    vi.useFakeTimers();
    let reject = true;
    const { r, dispose } = createRoot((d) => ({
      r: createSessionRewind({
        sessionId: () => "s1",
        onDone: () => {},
        call: () => (reject ? Promise.reject(DIRTY) : Promise.resolve()),
      }),
      dispose: d,
    }));
    await r.flow.request("m-9");
    expect(rewindPendingInfo()).toEqual({
      messageId: "m-9",
      dirtyCount: 2,
      targetRole: "user",
      targetPreview: "帮我改一下",
    });
    reject = false;
    await r.flow.confirm();
    expect(rewindPendingInfo()).toBeNull();
    dispose();
  });

  it("切会话清待确认条：旧 sid 的 pending 不泄漏（新 sid + 旧 mid 重发是误回退）", async () => {
    const [sid, setSid] = createSignal("s1");
    const { r, dispose } = createRoot((d) => ({
      r: createSessionRewind({
        sessionId: sid,
        onDone: () => {},
        call: () => Promise.reject(DIRTY),
      }),
      dispose: d,
    }));
    await r.flow.request("m-9");
    expect(r.pending()).toBe("m-9");
    expect(rewindPendingInfo()).not.toBeNull();
    setSid("s2");
    await new Promise((res) => setTimeout(res, 0)); // 等 createEffect 生效
    expect(r.pending()).toBeNull();
    expect(rewindPendingInfo()).toBeNull();
    dispose();
  });

  it("切会话时旧 rewind 仍在飞：迟到 dirty 不得重开旧会话确认条", async () => {
    const [sid, setSid] = createSignal("s1");
    let reject!: (error: unknown) => void;
    const { r, dispose } = createRoot((d) => ({
      r: createSessionRewind({
        sessionId: sid,
        onDone: () => {},
        call: () =>
          new Promise((_, fail) => {
            reject = fail;
          }),
      }),
      dispose: d,
    }));
    const request = r.flow.request("m-9");
    expect(r.flow.busy()).toBe(true);
    setSid("s2");
    await new Promise((res) => setTimeout(res, 0));
    expect(r.flow.busy()).toBe(false);
    reject(DIRTY);
    await request;
    expect(r.pending()).toBeNull();
    expect(rewindPendingInfo()).toBeNull();
    expect(r.note()).toBe("");
    dispose();
  });
});
