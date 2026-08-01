// 更新检查共享状态：并发去重不重复请求；启动静默检查失败静默、有更新只 toast 不弹窗。
// 浏览器模式 resetModules 不刷新模块：checked/flight 是模块级单例，用例按生命周期顺序断言。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  check: vi.fn(),
  relaunch: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/api/app", () => ({ getVersion: async () => "0.1.0" }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: h.relaunch }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: h.check }));

import { autoCheckOnStartup, availableUpdate, checkForUpdate } from "./updater";
import { flash } from "./flash";

beforeEach(() => {
  h.check.mockReset();
});

afterEach(() => {
  for (const m of flash.msgs()) flash.dismiss(m.id);
});

describe("updater 共享状态与启动静默检查", () => {
  it("autoCheckOnStartup 全生命周期：失败静默可重试 -> 有更新 toast 并填充状态 -> 之后不再请求", async () => {
    // 检查失败：不 toast 不外抛，availableUpdate 保持 null
    h.check.mockRejectedValue(new Error("offline"));
    autoCheckOnStartup();
    await vi.waitFor(() => expect(h.check).toHaveBeenCalledTimes(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(flash.msgs()).toEqual([]);
    expect(availableUpdate()).toBeNull();

    // 失败不置 checked：再次启动检查可重试；有更新则 toast + 填充共享状态
    const update = { version: "0.2.0", downloadAndInstall: vi.fn() };
    h.check.mockResolvedValue(update);
    autoCheckOnStartup();
    await vi.waitFor(() => expect(availableUpdate()).toEqual(update));
    expect(flash.msgs().some((m) => m.kind === "ok" && m.text.includes("0.2.0"))).toBe(true);

    // 已检查过：重复启动检查不再发请求
    autoCheckOnStartup();
    await new Promise((r) => setTimeout(r, 20));
    expect(h.check).toHaveBeenCalledTimes(2);
  });

  it("checkForUpdate 并发去重：共享同一 flight，无更新结果写入共享状态", async () => {
    h.check.mockResolvedValue(null);
    const callsBefore = h.check.mock.calls.length;
    const [a, b] = await Promise.all([checkForUpdate(), checkForUpdate()]);
    expect(h.check.mock.calls.length).toBe(callsBefore + 1);
    expect(a).toBeNull();
    expect(b).toBeNull();
    expect(availableUpdate()).toBeNull();
  });
});
