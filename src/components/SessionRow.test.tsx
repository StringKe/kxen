// SessionRow 三处小修回归：重命名 RPC 失败 finally 必退编辑态、置顶失败 flashErr、
// 删除确认态 mouseleave 复位。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionMeta } from "../lib/chat";

const h = vi.hoisted(() => ({
  sessionUpdateMeta: vi.fn(async (_id: string, _patch: unknown) => {}),
  currentModel: vi.fn(async (_id?: string) => ({ provider: "xai", model: "grok-4" })),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    currentModel: h.currentModel,
    sessionUpdateMeta: h.sessionUpdateMeta,
  };
});

import SessionRow from "./SessionRow";
import { flash } from "../lib/flash";
import { closeMenu, menu } from "../lib/context-menu";
import { setActiveSessionId } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));

const S: SessionMeta = { id: "s1", title: "旧标题", directory: "/a", created_at: 1, updated_at: 1 };

const renderRow = () =>
  render(
    () => (
      <SessionRow
        session={S}
        deleting={false}
        onOpen={() => {}}
        onDelete={() => {}}
        onChanged={() => {}}
        draggable={false}
        dropTarget={false}
        onDragStart={() => {}}
        onDragOver={() => {}}
        onDragLeave={() => {}}
        onDrop={() => {}}
        onDragEnd={() => {}}
      />
    ),
    document.body,
  );

const titleSpan = () =>
  [...document.body.querySelectorAll("span")].find((el) => el.textContent?.trim() === S.title);

afterEach(() => {
  closeMenu();
  document.body.innerHTML = "";
  for (const m of flash.msgs()) flash.dismiss(m.id);
  h.sessionUpdateMeta.mockReset();
  h.currentModel.mockReset();
  h.currentModel.mockResolvedValue({ provider: "xai", model: "grok-4" });
  setActiveSessionId("");
});

describe("SessionRow 失败路径", () => {
  it("重命名 RPC 失败：finally 退出编辑态并 flashErr（输入框不卡死）", async () => {
    h.sessionUpdateMeta.mockRejectedValue(new Error("connection lost"));
    const dispose = renderRow();
    titleSpan()?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    await flush();
    const input = document.body.querySelector("input");
    expect(input).not.toBeNull();
    input!.value = "新标题";
    input!.dispatchEvent(new Event("input", { bubbles: true }));
    input!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await flush();
    expect(document.body.querySelector("input")).toBeNull(); // finally 必退编辑态
    expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("重命名失败"))).toBe(true);
    dispose();
  });

  it("置顶 RPC 失败：flashErr 且不外抛", async () => {
    h.sessionUpdateMeta.mockRejectedValue(new Error("connection lost"));
    const dispose = renderRow();
    const pin = document.body.querySelector("button[title='置顶']");
    pin?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("置顶失败"))).toBe(true);
    dispose();
  });

  it("删除确认态：mouseleave 复位为未确认", async () => {
    const dispose = renderRow();
    const del = document.body.querySelector("button[title='删除会话（再点一次确认）']");
    del?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(document.body.querySelector("button[title='确认删除']")).not.toBeNull();
    document
      .querySelector(".interactive")!
      .dispatchEvent(new MouseEvent("mouseleave", { bubbles: false }));
    await flush();
    expect(document.body.querySelector("button[title='确认删除']")).toBeNull();
    expect(document.body.querySelector("button[title='删除会话（再点一次确认）']")).not.toBeNull();
    dispose();
  });

  it("右键菜单只有一项删除入口：直删/沉淀选择在行内确认条做", async () => {
    const dispose = renderRow();
    document
      .querySelector(".interactive")!
      .dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const dangerItems = (menu()?.items ?? []).filter((i) => i.danger);
    expect(dangerItems.map((i) => i.label)).toEqual(["删除会话..."]);
    dangerItems[0]!.action();
    await flush();
    expect(document.body.querySelector("button[title='确认删除']")).not.toBeNull();
    expect(document.body.querySelector("button[title*='沉淀为个人知识后删除']")).not.toBeNull();
    dispose();
  });

  it("删除并沉淀前显示实际 Provider、传输范围和个人知识范围", async () => {
    const dispose = renderRow();
    const del = document.body.querySelector("button[title='删除会话（再点一次确认）']");
    del?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await vi.waitFor(() => expect(h.currentModel).toHaveBeenCalledWith("s1"));
    const disclosure = document.body.querySelector("span[title*='最近文本']");
    expect(disclosure?.textContent).toContain("发送到 xai/grok-4");
    expect(disclosure?.getAttribute("title")).toContain("最近文本");
    expect(disclosure?.getAttribute("title")).toContain("只写个人知识");
    dispose();
  });
});

describe("SessionRow 完整交互", () => {
  it("运行中置顶行处理打开、拖拽、取消置顶、重命名取消和删除确认", async () => {
    h.sessionUpdateMeta.mockResolvedValue(undefined);
    setActiveSessionId("s1");
    const callbacks = {
      onOpen: vi.fn(),
      onDelete: vi.fn(),
      onChanged: vi.fn(),
      onDragStart: vi.fn(),
      onDragOver: vi.fn(),
      onDragLeave: vi.fn(),
      onDrop: vi.fn(),
      onDragEnd: vi.fn(),
    };
    const session = { ...S, pinned: true, running: true };
    const dispose = render(
      () => <SessionRow session={session} deleting={false} draggable dropTarget {...callbacks} />,
      document.body,
    );

    const row = document.body.querySelector<HTMLElement>(".interactive")!;
    row.click();
    row.dispatchEvent(new DragEvent("dragstart", { bubbles: true }));
    row.dispatchEvent(new DragEvent("dragover", { bubbles: true }));
    row.dispatchEvent(new DragEvent("dragleave", { bubbles: true }));
    row.dispatchEvent(new DragEvent("drop", { bubbles: true }));
    row.dispatchEvent(new DragEvent("dragend", { bubbles: true }));
    expect(callbacks.onOpen).toHaveBeenCalledTimes(1);
    expect(callbacks.onDragStart).toHaveBeenCalledTimes(1);
    expect(callbacks.onDragOver).toHaveBeenCalledTimes(1);
    expect(callbacks.onDragLeave).toHaveBeenCalledTimes(1);
    expect(callbacks.onDrop).toHaveBeenCalledTimes(1);
    expect(callbacks.onDragEnd).toHaveBeenCalledTimes(1);
    expect(row.className).toContain("shadow-");

    document.body.querySelector<HTMLButtonElement>("button[title='取消置顶']")?.click();
    await vi.waitFor(() =>
      expect(h.sessionUpdateMeta).toHaveBeenCalledWith("s1", { pinned: false }),
    );
    expect(callbacks.onChanged).toHaveBeenCalledTimes(1);

    titleSpan()?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    await flush();
    document.body
      .querySelector<HTMLInputElement>("input")
      ?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await flush();
    expect(document.body.querySelector("input")).toBeNull();

    document.body
      .querySelector<HTMLButtonElement>("button[title='删除会话（会话正在运行，删除将终止）']")
      ?.click();
    await flush();
    document.body
      .querySelector<HTMLButtonElement>("button[title='会话正在运行，删除将终止']")
      ?.click();
    expect(callbacks.onDelete).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("删除中状态禁用拖拽并显示 spinner", () => {
    const dispose = render(
      () => (
        <SessionRow
          session={S}
          deleting
          onOpen={() => {}}
          onDelete={() => {}}
          onChanged={() => {}}
          draggable
          dropTarget={false}
          onDragStart={() => {}}
          onDragOver={() => {}}
          onDragLeave={() => {}}
          onDrop={() => {}}
          onDragEnd={() => {}}
        />
      ),
      document.body,
    );
    const row = document.body.querySelector<HTMLElement>(".interactive");
    expect(row?.getAttribute("draggable")).toBe("false");
    expect(document.body.querySelector("[title='删除中…']")).toBeTruthy();
    dispose();
  });
});
