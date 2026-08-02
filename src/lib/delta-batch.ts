// delta 批量上屏：50ms 窗口合并文本增量。
// 每 delta 全量 setItems = 全量 markdown 重解析 + DOM 重建 = 滚动闪烁的根因之一。

export function createDeltaBatcher(
  appendRaw: (field: "content" | "reasoning", text: string) => void,
  ms = 50,
) {
  const buffers: Record<"content" | "reasoning", string> = { content: "", reasoning: "" };
  let timer: ReturnType<typeof setTimeout> | undefined;

  const flush = () => {
    timer = undefined;
    const c = buffers.content;
    const r = buffers.reasoning;
    buffers.content = "";
    buffers.reasoning = "";
    if (c) appendRaw("content", c);
    if (r) appendRaw("reasoning", r);
  };

  return {
    push(field: "content" | "reasoning", text: string) {
      buffers[field] += text;
      if (!timer) timer = setTimeout(flush, ms);
    },
    /** 终态前调用：残余增量立即上屏。 */
    flushNow() {
      if (timer) flush();
    },
  };
}
