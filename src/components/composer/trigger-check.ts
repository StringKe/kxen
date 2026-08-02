// 触发词防抖检出 + 弹层装配：200ms 合并连续输入，命中后算锚点开弹层（无命中/空结果即关）。
import type { CommandInfo } from "../../lib/chat";
import { errText } from "../err-text";
import { buildItems, detectTrigger, type PopupState, type Trigger } from "./triggers";
import type { RowChip } from "./RowChips";

export function createTriggerCheck(opts: {
  ta: () => HTMLTextAreaElement | undefined;
  text: () => string;
  commands: () => CommandInfo[];
  commandsError: () => string;
  retryCommands: () => Promise<void>;
  removeTriggerText: (trigger: Trigger, from?: number) => void;
  pushChip: (chip: Omit<RowChip, "id">) => void;
  insertAtCaret: (insert: string) => void;
  setPopup: (p: (PopupState & Trigger) | null) => void;
  updatePopupPos: () => void;
}): { run: () => void; dispose: () => void } {
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  let generation = 0;
  let lastGood: { key: string; items: PopupState["items"] } | undefined;

  const matchesAt = (expected: Trigger, cursor: number): boolean => {
    const current = detectTrigger(opts.text(), cursor);
    return (
      current?.kind === expected.kind &&
      current.start === expected.start &&
      current.query === expected.query
    );
  };
  const matchesCaret = (expected: Trigger): boolean => {
    const ta = opts.ta();
    return matchesAt(expected, ta?.selectionStart ?? opts.text().length);
  };
  // popup 点击会让 textarea caret 移动；apply 只需确认原触发段文本没变，不能要求 caret 仍在段内。
  const matchesText = (expected: Trigger): boolean =>
    matchesAt(expected, expected.start + 1 + expected.query.length);

  function run() {
    const request = ++generation;
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(async () => {
      debounceTimer = undefined;
      if (request !== generation) return;
      const ta = opts.ta();
      const cursor = ta?.selectionStart ?? opts.text().length;
      const trigger = detectTrigger(opts.text(), cursor);
      if (!trigger) {
        if (request === generation) opts.setPopup(null);
        return;
      }
      const key = `${trigger.kind}\u0000${trigger.start}\u0000${trigger.query}`;
      let items: PopupState["items"];
      try {
        items = await buildItems(trigger, opts.commands(), {
          onChip: (kind, ref, label) => {
            if (!matchesText(trigger)) return;
            opts.removeTriggerText(trigger);
            opts.pushChip({ kind, ref, label, title: ref });
            opts.setPopup(null);
            ta?.focus();
          },
          onPlainInsert: (insert, start) => {
            if (!matchesText(trigger)) return;
            opts.removeTriggerText(trigger, start);
            opts.insertAtCaret(insert);
            opts.setPopup(null);
            ta?.focus();
          },
        });
        if (request !== generation || !matchesCaret(trigger)) return;
        if (items.length > 0) lastGood = { key, items };
        else if (lastGood?.key === key) lastGood = undefined;
      } catch (error) {
        if (request !== generation || !matchesCaret(trigger)) return;
        const previous = lastGood?.key === key ? lastGood.items : [];
        items = [
          retryItem(
            `${previous.length > 0 ? "文件补全刷新失败，正在显示上次结果" : "文件补全失败"}：${errText(error)}`,
            run,
          ),
          ...previous,
        ];
      }
      if (request !== generation || !matchesCaret(trigger)) return;
      if (trigger.kind === "slash" && opts.commandsError()) {
        items = [
          retryItem(
            `${items.length > 0 ? "命令清单刷新失败，正在显示上次结果" : "命令清单加载失败"}：${opts.commandsError()}`,
            () => void opts.retryCommands(),
          ),
          ...items,
        ];
      }
      if (items.length === 0) {
        opts.setPopup(null);
        return;
      }
      opts.updatePopupPos();
      opts.setPopup({ ...trigger, items, selected: 0 });
    }, 200);
  }

  const retryItem = (message: string, retry: () => void): PopupState["items"][number] => ({
    label: `UNKNOWN：${message}`,
    detail: "选择此项重试",
    tone: "error",
    apply: () => {
      retry();
      opts.ta()?.focus();
    },
  });

  return {
    run,
    dispose: () => {
      generation++;
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = undefined;
    },
  };
}
