import { afterEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({ buildItems: vi.fn() }));

vi.mock("./triggers", async (importOriginal) => {
  const original = await importOriginal<typeof import("./triggers")>();
  return { ...original, buildItems: h.buildItems };
});

import { createTriggerCheck } from "./trigger-check";
import type { PopupState, Trigger } from "./triggers";

afterEach(() => {
  vi.useRealTimers();
  h.buildItems.mockReset();
  document.body.innerHTML = "";
});

describe("trigger async generation", () => {
  it("旧 query 的慢结果不能覆盖新 query popup", async () => {
    vi.useFakeTimers();
    const ta = document.createElement("textarea");
    document.body.append(ta);
    let text = "@a";
    ta.value = text;
    ta.setSelectionRange(text.length, text.length);
    let popup: (PopupState & Trigger) | null = null;
    const resolvers: Array<(items: Array<{ label: string; apply: () => void }>) => void> = [];
    h.buildItems.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolvers.push(resolve);
        }),
    );
    const check = createTriggerCheck({
      ta: () => ta,
      text: () => text,
      commands: () => [],
      commandsError: () => "",
      retryCommands: async () => {},
      removeTriggerText: vi.fn(),
      pushChip: vi.fn(),
      insertAtCaret: vi.fn(),
      setPopup: (next) => (popup = next),
      updatePopupPos: vi.fn(),
    });

    check.run();
    await vi.advanceTimersByTimeAsync(200);
    text = "@ab";
    ta.value = text;
    ta.setSelectionRange(text.length, text.length);
    check.run();
    await vi.advanceTimersByTimeAsync(200);

    resolvers[1]?.([{ label: "new", apply: () => {} }]);
    await Promise.resolve();
    expect((popup as (PopupState & Trigger) | null)?.items[0]?.label).toBe("new");

    resolvers[0]?.([{ label: "old", apply: () => {} }]);
    await Promise.resolve();
    expect((popup as (PopupState & Trigger) | null)?.items[0]?.label).toBe("new");
    check.dispose();
  });

  it("旧 query 的失败不得覆盖新 query 已成功的 popup", async () => {
    vi.useFakeTimers();
    const ta = document.createElement("textarea");
    document.body.append(ta);
    let text = "@a";
    ta.value = text;
    ta.setSelectionRange(text.length, text.length);
    let popup: (PopupState & Trigger) | null = null;
    let rejectOld!: (error: unknown) => void;
    h.buildItems
      .mockReturnValueOnce(new Promise((_resolve, reject) => (rejectOld = reject)))
      .mockResolvedValueOnce([{ label: "new", apply: () => {} }]);
    const check = createTriggerCheck({
      ta: () => ta,
      text: () => text,
      commands: () => [],
      commandsError: () => "",
      retryCommands: async () => {},
      removeTriggerText: vi.fn(),
      pushChip: vi.fn(),
      insertAtCaret: vi.fn(),
      setPopup: (next) => (popup = next),
      updatePopupPos: vi.fn(),
    });
    check.run();
    await vi.advanceTimersByTimeAsync(200);
    text = "@ab";
    ta.value = text;
    ta.setSelectionRange(text.length, text.length);
    check.run();
    await vi.advanceTimersByTimeAsync(200);
    expect((popup as (PopupState & Trigger) | null)?.items[0]?.label).toBe("new");
    rejectOld(new Error("old offline"));
    await Promise.resolve();
    expect((popup as (PopupState & Trigger) | null)?.items[0]?.label).toBe("new");
    check.dispose();
  });
});
