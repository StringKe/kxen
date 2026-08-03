// 拖拽载荷分流：enter/over 悬停开、leave 关、drop 出路径；空 paths 的 drop 只复位 hover。
import { beforeEach, describe, expect, it, vi } from "vitest";
import { dragEffect, listenComposerDragDrop } from "./drag-drop";

const webviewMock = vi.hoisted(() => ({
  listen: vi.fn(),
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: webviewMock.listen }),
}));

beforeEach(() => {
  webviewMock.listen.mockReset();
  webviewMock.listen.mockResolvedValue(() => {});
});

describe("dragEffect", () => {
  it("enter/over 开悬停高亮", () => {
    expect(dragEffect({ type: "enter", paths: ["/a.png"] })).toEqual({ kind: "hover", on: true });
    expect(dragEffect({ type: "over" })).toEqual({ kind: "hover", on: true });
  });

  it("leave 关悬停高亮", () => {
    expect(dragEffect({ type: "leave" })).toEqual({ kind: "hover", on: false });
  });

  it("drop 带路径出 attach 动作", () => {
    expect(dragEffect({ type: "drop", paths: ["/a.png", "/b.txt"] })).toEqual({
      kind: "drop",
      paths: ["/a.png", "/b.txt"],
    });
  });

  it("drop 空路径只复位 hover（不触发空 attach）", () => {
    expect(dragEffect({ type: "drop", paths: [] })).toEqual({ kind: "hover", on: false });
    expect(dragEffect({ type: "drop" })).toEqual({ kind: "hover", on: false });
  });

  it("未知类型忽略", () => {
    expect(dragEffect({ type: "hover" })).toEqual({ kind: "none" });
  });
});

describe("listenComposerDragDrop", () => {
  it("注册失败不炸，返回可调的注销函数", () => {
    const un = listenComposerDragDrop(
      () => {},
      () => {},
    );
    expect(typeof un).toBe("function");
    expect(() => un()).not.toThrow();
  });

  it("注册 Promise 落定前 cleanup：迟到的 unlisten 立即执行", async () => {
    let resolveListen!: (unlisten: () => void) => void;
    webviewMock.listen.mockReturnValue(
      new Promise((resolve) => {
        resolveListen = resolve;
      }),
    );
    const lateUnlisten = vi.fn();
    const cleanup = listenComposerDragDrop(
      () => {},
      () => {},
    );

    cleanup();
    resolveListen(lateUnlisten);
    await vi.waitFor(() => expect(lateUnlisten).toHaveBeenCalledOnce());
  });
});
