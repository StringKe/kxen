// UserItem：右键「编辑并重发」与铅笔同一编辑框入口（旧右键跳过编辑框直接原文重发）；
// 无 messageId 的乐观消息 rewind 禁用；图片 load 回调（宿主据此重钉底）。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import { closeMenu, menu } from "../lib/context-menu";
import type { MsgItem } from "../lib/items";
import { clearSessionMessageEditDrafts } from "../lib/message-edit-drafts";
import UserItem from "./UserItem";

const base = (extra: Partial<MsgItem> = {}): MsgItem => ({
  kind: "msg",
  role: "user",
  content: "原文",
  messageId: "u1",
  ...extra,
});

function setup(
  itemProps: MsgItem,
  onEditResend: (t: string) => Promise<boolean> = async () => true,
  onImageLoad?: () => void,
  onRetry: () => void = () => {},
  retrying: () => boolean = () => false,
) {
  return render(
    () => (
      <UserItem
        item={itemProps}
        sessionId={() => "s1"}
        onFork={() => {}}
        onEditResend={onEditResend}
        onRewind={() => {}}
        onRetry={onRetry}
        retrying={retrying}
        {...(onImageLoad ? { onImageLoad } : {})}
      />
    ),
    document.body,
  );
}

function openContextMenu() {
  const el = document.body.querySelector(".group");
  if (!el) throw new Error("UserItem 未渲染");
  el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 10, clientY: 10 }));
  return menu()?.items ?? [];
}

afterEach(() => {
  closeMenu();
  clearSessionMessageEditDrafts("s1");
  document.body.innerHTML = "";
});

describe("编辑并重发入口一致", () => {
  it("右键进编辑框（预填原文）不直接重发；提交走编辑后文本", async () => {
    const editResend = vi.fn(async () => true);
    setup(base(), editResend);
    const edit = openContextMenu().find((i) => i.label === "编辑并重发");
    expect(edit).toBeTruthy();
    edit!.action();
    closeMenu();
    const ta = document.body.querySelector("textarea");
    expect(ta).toBeTruthy();
    expect(ta!.value).toBe("原文");
    expect(editResend).not.toHaveBeenCalled();

    ta!.value = "改过的文本";
    ta!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    const submit = [...document.body.querySelectorAll("button")].find(
      (b) => b.textContent === "重发（开分支）",
    );
    submit!.click();
    await vi.waitFor(() => expect(editResend).toHaveBeenCalledWith("改过的文本"));
    expect(document.body.querySelector("textarea")).toBeNull();
  });

  it("编辑重发未准入时保留编辑文本供重试", async () => {
    setup(base(), async () => false);
    openContextMenu()
      .find((item) => item.label === "编辑并重发")!
      .action();
    closeMenu();
    const textarea = document.body.querySelector<HTMLTextAreaElement>("textarea")!;
    textarea.value = "不能丢的编辑";
    textarea.dispatchEvent(new InputEvent("input", { bubbles: true }));
    [...document.body.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent === "重发（开分支）")!
      .click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(document.body.querySelector<HTMLTextAreaElement>("textarea")?.value).toBe(
      "不能丢的编辑",
    );
  });

  it("编辑重发在飞时锁定文本并去重提交", async () => {
    let finish!: (admitted: boolean) => void;
    const resend = vi.fn(() => new Promise<boolean>((resolve) => (finish = resolve)));
    setup(base(), resend);
    openContextMenu()
      .find((item) => item.label === "编辑并重发")!
      .action();
    closeMenu();
    const submit = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "重发（开分支）",
    )!;
    submit.click();
    submit.click();
    await vi.waitFor(() => expect(resend).toHaveBeenCalledTimes(1));
    expect(document.body.querySelector<HTMLTextAreaElement>("textarea")?.disabled).toBe(true);
    finish(false);
    await vi.waitFor(() =>
      expect(document.body.querySelector<HTMLTextAreaElement>("textarea")?.disabled).toBe(false),
    );
  });

  it("时间线快照替换组件时保留编辑文本和在飞锁，完成后解除锁定", async () => {
    let finish!: (admitted: boolean) => void;
    const resend = vi.fn(() => new Promise<boolean>((resolve) => (finish = resolve)));
    const first = setup(base(), resend);
    openContextMenu()
      .find((item) => item.label === "编辑并重发")!
      .action();
    closeMenu();
    const textarea = document.body.querySelector<HTMLTextAreaElement>("textarea")!;
    textarea.value = "跨快照保留的编辑";
    textarea.dispatchEvent(new InputEvent("input", { bubbles: true }));
    [...document.body.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent === "重发（开分支）")!
      .click();
    await vi.waitFor(() => expect(textarea.disabled).toBe(true));

    first();
    const second = setup(base({ content: "服务端新快照" }), resend);
    const restored = document.body.querySelector<HTMLTextAreaElement>("textarea")!;
    expect(restored.value).toBe("跨快照保留的编辑");
    expect(restored.disabled).toBe(true);

    finish(false);
    await vi.waitFor(() => expect(restored.disabled).toBe(false));
    expect(restored.value).toBe("跨快照保留的编辑");
    second();
  });
});

describe("rewind 入口", () => {
  it("有 messageId：可用", () => {
    setup(base());
    const rewind = openContextMenu().find((i) => i.label === "回退到此处");
    expect(rewind?.disabled).toBe(false);
  });

  it("无 messageId（未持久化乐观消息）：禁用（点了只会报 missing message_id）", () => {
    setup(base({ messageId: undefined }));
    const rewind = openContextMenu().find((i) => i.label === "回退到此处");
    expect(rewind?.disabled).toBe(true);
  });
});

describe("图片附件", () => {
  it("图片 load 后回调 onImageLoad（异步解码撑高列表，宿主重钉底）", () => {
    const onImageLoad = vi.fn();
    setup(
      base({ images: [{ media_type: "image/png", data: "QUJD" }] }),
      async () => true,
      onImageLoad,
    );
    const img = document.body.querySelector("img");
    expect(img).toBeTruthy();
    img!.dispatchEvent(new Event("load"));
    expect(onImageLoad).toHaveBeenCalledTimes(1);
  });
});

describe("发送失败状态", () => {
  it("UNKNOWN 结果不提供一键重发", () => {
    const retry = vi.fn();
    setup(
      base({ sendError: "connection lost", sendOutcome: "unknown" }),
      async () => true,
      undefined,
      retry,
    );
    expect(document.body.textContent).toContain("发送结果 UNKNOWN");
    expect(document.body.textContent).toContain("避免重复发送");
    expect(document.body.querySelector('button[title="点击重发"]')).toBeNull();
  });

  it("确定失败重发在飞时禁用按钮", () => {
    setup(
      base({ sendError: "rejected", sendOutcome: "failed" }),
      async () => true,
      undefined,
      () => {},
      () => true,
    );
    const retry = document.body.querySelector<HTMLButtonElement>('button[title="点击重发"]')!;
    expect(retry.disabled).toBe(true);
    expect(retry.textContent).toContain("正在重发");
  });
});
