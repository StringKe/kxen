import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KnowledgeEntry } from "../../lib/knowledge";

const h = vi.hoisted(() => ({
  add: vi.fn(async () => {}),
  list: vi.fn(async () => [] as KnowledgeEntry[]),
  move: vi.fn(async () => {}),
  preview: vi.fn(async () => ({ block: "injected knowledge" })),
  remove: vi.fn(async () => {}),
  setEnabled: vi.fn(async () => {}),
}));

vi.mock("../../lib/knowledge", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../lib/knowledge")>();
  return {
    ...original,
    knowledgeAdd: h.add,
    knowledgeInjectionPreview: h.preview,
    knowledgeList: h.list,
    knowledgeMove: h.move,
    knowledgeRemove: h.remove,
    knowledgeSetEnabled: h.setEnabled,
  };
});

vi.mock("./CodingRulesBlock", () => ({ default: () => <div>coding rules</div> }));

import KnowledgeSection from "./KnowledgeSection";
import { flash } from "../../lib/flash";

const entry = (overrides: Partial<KnowledgeEntry>): KnowledgeEntry => ({
  scope: "project",
  kind: "rule",
  slug: "shared",
  description: "共享规则",
  content: "规则正文",
  path: "/knowledge/shared.md",
  enabled: true,
  always_apply: false,
  globs: [],
  needs: [],
  note_type: "convention",
  date: "2026-07-27",
  ...overrides,
});

const entries = [
  entry({ scope: "project", always_apply: true }),
  entry({
    scope: "personal",
    description: "被覆盖规则",
    content: "个人正文",
  }),
  entry({
    scope: "personal",
    kind: "skill",
    slug: "disabled-skill",
    description: "停用技能",
    enabled: false,
  }),
];

function buttonByText(text: string): HTMLButtonElement {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
    item.textContent?.includes(text),
  );
  if (!button) throw new Error(`button not found: ${text}`);
  return button;
}

beforeEach(() => {
  h.list.mockReset();
  h.list.mockResolvedValue(entries);
  h.preview.mockReset();
  h.preview.mockResolvedValue({ block: "injected knowledge" });
});

afterEach(() => {
  document.body.innerHTML = "";
  for (const message of flash.msgs()) flash.dismiss(message.id);
  vi.clearAllMocks();
});

describe("KnowledgeSection 生命周期", () => {
  it("加载、预览、启停、移动、删除和新增均使用统一知识接口", async () => {
    const dispose = render(() => <KnowledgeSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("被项目覆盖"));
    expect(document.body.textContent).toContain("always");
    expect(document.body.textContent).toContain("项目 1/1");
    expect(document.body.textContent).toContain("个人 1/2");

    buttonByText("注入预览").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("injected knowledge"));

    document.body.querySelector<HTMLButtonElement>("button[title='停用（注入即刻跳过）']")?.click();
    await vi.waitFor(() => expect(h.setEnabled).toHaveBeenCalledWith("project", "shared", false));

    const moveSelect = document.body.querySelector<HTMLSelectElement>(
      "select[title='晋升/降级（跨 scope 移动，保 kind）']",
    );
    expect(moveSelect).toBeTruthy();
    moveSelect!.value = "personal";
    moveSelect!.dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() => expect(h.move).toHaveBeenCalledWith("project", "shared", "personal"));

    document.body.querySelector<HTMLButtonElement>("button[title='删除（废纸篓可恢复）']")?.click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("确认删除"));
    expect(h.remove).not.toHaveBeenCalled(); // 一键不直删：先出行内确认条
    buttonByText("确认删除").click();
    await vi.waitFor(() => expect(h.remove).toHaveBeenCalledWith("project", "shared"));

    const scopeSelect = [...document.body.querySelectorAll<HTMLSelectElement>("select")].find(
      (select) => select.options[0]?.textContent?.includes("个人（默认）"),
    );
    scopeSelect!.value = "project";
    scopeSelect!.dispatchEvent(new Event("change", { bubbles: true }));
    const noteSelect = [...document.body.querySelectorAll<HTMLSelectElement>("select")].find(
      (select) => select.options[0]?.value === "correction",
    );
    noteSelect!.value = "pitfall";
    noteSelect!.dispatchEvent(new Event("change", { bubbles: true }));

    const description = document.body.querySelector<HTMLInputElement>(
      "input[placeholder^='一句话描述']",
    );
    const content = document.body.querySelector<HTMLTextAreaElement>("textarea");
    description!.value = "部署陷阱";
    description!.dispatchEvent(new Event("input", { bubbles: true }));
    content!.value = "必须先校验签名";
    content!.dispatchEvent(new Event("input", { bubbles: true }));
    buttonByText("写入知识库").click();
    await vi.waitFor(() =>
      expect(h.add).toHaveBeenCalledWith("project", "pitfall", "部署陷阱", "必须先校验签名"),
    );
    await vi.waitFor(() =>
      expect(flash.msgs().some((message) => message.kind === "ok")).toBe(true),
    );
    dispose();
  });

  it("写入、启停、移动和删除失败时保留现场并显示具体错误", async () => {
    h.add.mockRejectedValueOnce(new Error("add failed"));
    h.setEnabled.mockRejectedValueOnce(new Error("toggle failed"));
    h.move.mockRejectedValueOnce(new Error("move failed"));
    h.remove.mockRejectedValueOnce(new Error("remove failed"));
    const dispose = render(() => <KnowledgeSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("共享规则"));

    document.body.querySelector<HTMLButtonElement>("button[title='停用（注入即刻跳过）']")?.click();
    await vi.waitFor(() =>
      expect(flash.msgs().some((message) => message.text.includes("toggle failed"))).toBe(true),
    );
    const moveSelect = document.body.querySelector<HTMLSelectElement>(
      "select[title='晋升/降级（跨 scope 移动，保 kind）']",
    );
    moveSelect!.value = "personal";
    moveSelect!.dispatchEvent(new Event("change", { bubbles: true }));
    document.body.querySelector<HTMLButtonElement>("button[title='删除（废纸篓可恢复）']")?.click();
    buttonByText("确认删除").click();

    const description = document.body.querySelector<HTMLInputElement>(
      "input[placeholder^='一句话描述']",
    );
    const content = document.body.querySelector<HTMLTextAreaElement>("textarea");
    description!.value = "失败笔记";
    description!.dispatchEvent(new Event("input", { bubbles: true }));
    content!.value = "保留正文";
    content!.dispatchEvent(new Event("input", { bubbles: true }));
    buttonByText("写入知识库").click();

    await vi.waitFor(() => {
      const errors = flash
        .msgs()
        .filter((message) => message.kind === "err")
        .map((message) => message.text);
      expect(errors).toEqual(
        expect.arrayContaining([
          expect.stringContaining("move failed"),
          expect.stringContaining("remove failed"),
          expect.stringContaining("add failed"),
        ]),
      );
    });
    expect(description?.value).toBe("失败笔记");
    expect(content?.value).toBe("保留正文");
    dispose();
  });

  it("双源加载失败显示 UNKNOWN，不伪装成空知识库", async () => {
    h.list.mockRejectedValueOnce(new Error("list failed"));
    h.preview.mockRejectedValueOnce(new Error("preview failed"));
    const dispose = render(() => <KnowledgeSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("知识列表读取失败"));
    expect(document.body.textContent).toContain("知识条目统计 UNKNOWN");
    expect(document.body.textContent).not.toContain("暂无项目知识");
    expect(document.body.textContent).not.toContain("暂无个人知识");
    buttonByText("注入预览").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("注入预览 UNKNOWN"));
    expect(document.body.textContent).toContain("preview failed");
    dispose();
  });

  it("刷新失败保留最后一次成功列表", async () => {
    const dispose = render(() => <KnowledgeSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("共享规则"));
    h.list.mockRejectedValueOnce(new Error("refresh failed"));

    document.body.querySelector<HTMLButtonElement>("button[title='停用（注入即刻跳过）']")?.click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("refresh failed"));
    expect(document.body.textContent).toContain("共享规则");
    dispose();
  });
});
