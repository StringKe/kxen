// ScheduleSection 回归：暂停 job 不显示「下次」（next_fire 是暂停前陈旧值）；
// prompt 截断带 title 悬停全文；toggle/remove 失败 flashErr 带原因。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ScheduleJob } from "../../lib/schedule";

const h = vi.hoisted(() => ({
  list: vi.fn(async () => [] as ScheduleJob[]),
  add: vi.fn(async () => JOB({})),
  setEnabled: vi.fn(async (_id: string, _enabled: boolean) => true),
  remove: vi.fn(async (_id: string) => true),
}));

vi.mock("../../lib/schedule", () => ({
  scheduleList: h.list,
  scheduleAdd: h.add,
  scheduleSetEnabled: h.setEnabled,
  scheduleRemove: h.remove,
}));

import ScheduleSection from "./ScheduleSection";
import { flash } from "../../lib/flash";

const JOB = (over: Partial<ScheduleJob>): ScheduleJob => ({
  id: "j1",
  cron: "0 9 * * *",
  prompt: "把今天的未读通知汇总成三行，附上链接与优先级判断，超过两百字也要完整悬停可见",
  session_id: "sess-1",
  once: false,
  next_fire: Date.now() + 3600_000,
  enabled: true,
  history: [],
  ...over,
});

function btnByText(text: string): HTMLButtonElement {
  const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  );
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

afterEach(() => {
  document.body.innerHTML = "";
  for (const m of flash.msgs()) flash.dismiss(m.id);
  vi.clearAllMocks();
});

describe("ScheduleSection 展示", () => {
  it("启用 job 显示下次；暂停 job 不显示下次", async () => {
    h.list.mockResolvedValue([JOB({}), JOB({ id: "j2", enabled: false })]);
    const dispose = render(() => <ScheduleSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("下次"));
    const rows = [...document.body.querySelectorAll("div.px-4.py-3")];
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("下次");
    expect(rows[1]!.textContent).not.toContain("下次");
    expect(rows[1]!.textContent).toContain("已暂停");
    dispose();
  });

  it("prompt 截断带 title 全文", async () => {
    const job = JOB({});
    h.list.mockResolvedValue([job]);
    const dispose = render(() => <ScheduleSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain(job.prompt));
    const el = [...document.body.querySelectorAll<HTMLElement>(".truncate")].find(
      (n) => n.title === job.prompt,
    );
    expect(el).toBeTruthy();
    dispose();
  });
});

describe("ScheduleSection 删除确认", () => {
  it("删除先出行内确认条：未确认不发 RPC，确认后失败 flashErr", async () => {
    h.list.mockResolvedValue([JOB({})]);
    h.remove.mockRejectedValue(new Error("job busy"));
    const dispose = render(() => <ScheduleSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("删除"));
    btnByText("删除").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("确认删除定时任务"));
    expect(h.remove).not.toHaveBeenCalled(); // 未确认不发 RPC

    btnByText("确认删除").click();
    await vi.waitFor(() => {
      const err = flash.msgs().find((m) => m.kind === "err");
      expect(err?.text).toContain("删除失败");
      expect(err?.text).toContain("job busy");
    });
    expect(h.remove).toHaveBeenCalledWith("j1");
    expect(document.body.textContent).not.toContain("确认删除定时任务"); // 确认后条收起
    dispose();
  });

  it("取消确认条不发 RPC 且收起", async () => {
    h.list.mockResolvedValue([JOB({})]);
    const dispose = render(() => <ScheduleSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("删除"));
    btnByText("删除").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("确认删除定时任务"));
    btnByText("取消").click();
    await vi.waitFor(() => expect(document.body.textContent).not.toContain("确认删除定时任务"));
    expect(h.remove).not.toHaveBeenCalled();
    dispose();
  });
});

describe("ScheduleSection 操作失败", () => {
  it("toggle 失败 flashErr，不刷新伪装成功", async () => {
    h.list.mockResolvedValue([JOB({})]);
    h.setEnabled.mockRejectedValue(new Error("store locked"));
    const dispose = render(() => <ScheduleSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("暂停"));
    btnByText("暂停").click();
    await vi.waitFor(() => {
      const err = flash.msgs().find((m) => m.kind === "err");
      expect(err?.text).toContain("暂停失败");
      expect(err?.text).toContain("store locked");
    });
    expect(h.list).toHaveBeenCalledTimes(1); // 失败不触发 reload
    dispose();
  });
});
