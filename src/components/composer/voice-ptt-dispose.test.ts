import { describe, expect, it, vi } from "vitest";
import type { VoiceSession } from "../../lib/voice";
import { createVoicePtt } from "./voice-ptt";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

function setup(startSession: Parameters<typeof createVoicePtt>[0]["startSession"]) {
  let text = "草稿";
  let recording = false;
  const errors: string[] = [];
  const ctl = createVoicePtt({
    getText: () => text,
    setText: (value) => {
      text = value;
    },
    afterChange: () => {},
    setRecording: (value) => {
      recording = value;
    },
    setError: (value) => errors.push(value),
    engine: () => "apple",
    ...(startSession ? { startSession } : {}),
  });
  return {
    ctl,
    text: () => text,
    setText: (value: string) => {
      text = value;
    },
    recording: () => recording,
    errors,
  };
}

describe("voice PTT dispose", () => {
  it("录音期间开头与 transcript 中间人工编辑：next partial/final 保持位置且不丢内容", async () => {
    const finalText = deferred<string | null>();
    let partial: (text: string) => void = () => {};
    const h = setup(async (_engine, onPartial) => {
      partial = onPartial;
      return { engine: "apple", stop: () => finalText.promise };
    });
    h.ctl.toggle();
    await vi.waitFor(() => expect(h.recording()).toBe(true));
    partial("语音");
    expect(h.text()).toBe("草稿语音");

    h.setText("前草稿语音");
    partial("语音更新");
    expect(h.text()).toBe("前草稿语音更新");
    h.setText("前草稿语Y音更新");
    const stopping = h.ctl.stop();
    finalText.resolve("语音终稿");
    await stopping;
    expect(h.text()).toBe("前草稿语Y音终稿");
  });

  it("active session 卸载时只停止一次并丢弃终稿与迟到 partial", async () => {
    let stopped = 0;
    let partial: (text: string) => void = () => {};
    const h = setup(async (_engine, onPartial) => {
      partial = onPartial;
      return {
        engine: "apple",
        stop: async () => {
          stopped++;
          return "不应回填的终稿";
        },
      };
    });
    h.ctl.toggle();
    await vi.waitFor(() => expect(h.recording()).toBe(true));
    partial("已上屏 partial");
    expect(h.text()).toBe("草稿已上屏 partial");

    h.ctl.dispose();
    h.ctl.dispose();
    expect(h.recording()).toBe(false);
    await vi.waitFor(() => expect(stopped).toBe(1));
    partial("迟到 partial");
    expect(h.text()).toBe("草稿已上屏 partial");
  });

  it("starting session 卸载后，迟到启动结果立即自停且不回写 UI", async () => {
    const start = deferred<VoiceSession>();
    let stopped = 0;
    let partial: (text: string) => void = () => {};
    const h = setup((_engine, onPartial) => {
      partial = onPartial;
      return start.promise;
    });
    h.ctl.toggle();
    expect(h.ctl.starting()).toBe(true);
    h.ctl.dispose();
    partial("卸载后 partial");
    expect(h.text()).toBe("草稿");

    start.resolve({
      engine: "apple",
      stop: async () => {
        stopped++;
        return "迟到终稿";
      },
    });
    await vi.waitFor(() => expect(stopped).toBe(1));
    expect(h.recording()).toBe(false);
    expect(h.text()).toBe("草稿");
    expect(h.errors).toEqual([""]);
  });

  it("merge stop 在飞时的 discard 会使旧终稿失效", async () => {
    const finalText = deferred<string | null>();
    let stopped = 0;
    const h = setup(async () => ({
      engine: "apple",
      stop: () => {
        stopped++;
        return finalText.promise;
      },
    }));
    h.ctl.toggle();
    await vi.waitFor(() => expect(h.recording()).toBe(true));
    const merging = h.ctl.stop();
    await vi.waitFor(() => expect(stopped).toBe(1));
    void h.ctl.stop("discard");
    finalText.resolve("旧会话终稿");
    await merging;
    expect(h.text()).toBe("草稿");
  });

  it("主动 stop 在飞时 settle 返回同一 flight 并等待终稿合并", async () => {
    const finalText = deferred<string | null>();
    const h = setup(async () => ({
      engine: "apple",
      stop: () => finalText.promise,
    }));
    h.ctl.toggle();
    await vi.waitFor(() => expect(h.recording()).toBe(true));

    const stopping = h.ctl.stop();
    expect(h.ctl.settle()).toBe(stopping);
    let settled = false;
    void h.ctl.settle().then(() => {
      settled = true;
    });
    await Promise.resolve();
    expect(settled).toBe(false);

    finalText.resolve("终稿");
    await stopping;
    expect(settled).toBe(true);
    expect(h.text()).toBe("草稿终稿");
  });

  it("新录音启动后旧 stop 的终稿不能覆盖新 generation", async () => {
    const oldFinal = deferred<string | null>();
    let starts = 0;
    const h = setup(async () => {
      starts++;
      return {
        engine: "apple",
        stop: () => (starts === 1 ? oldFinal.promise : Promise.resolve(null)),
      };
    });
    h.ctl.toggle();
    await vi.waitFor(() => expect(h.recording()).toBe(true));
    const oldStop = h.ctl.stop();
    await vi.waitFor(() => expect(h.recording()).toBe(false));
    h.ctl.toggle();
    await vi.waitFor(() => expect(starts).toBe(2));
    oldFinal.resolve("旧终稿");
    await oldStop;
    expect(h.text()).toBe("草稿");
    h.ctl.dispose();
  });

  it("discard 后旧 start rejection 不写入新会话错误", async () => {
    const start = deferred<VoiceSession>();
    const h = setup(() => start.promise);
    h.ctl.toggle();
    const discarded = h.ctl.stop("discard");
    start.reject(new Error("旧会话权限失败"));
    await discarded;
    expect(h.errors).toEqual([""]);
    h.ctl.dispose();
  });
});
