import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  rpc: vi.fn(),
}));

vi.mock("./client", () => ({ client: { rpc: h.rpc } }));

beforeEach(() => {
  vi.resetModules();
  h.rpc.mockReset();
});

describe("modelsCatalog cache", () => {
  it("RPC 失败向调用方暴露且不缓存空目录", async () => {
    h.rpc.mockRejectedValueOnce(new Error("catalog unavailable"));
    const first = await import("./models");
    await expect(first.modelsCatalog()).rejects.toThrow("catalog unavailable");

    const catalog = [
      { provider: "p", provider_name: "P", models: [], fetched_at: 1, source: "builtin" },
    ];
    h.rpc.mockResolvedValueOnce(catalog);
    await expect(first.modelsCatalog()).resolves.toEqual(catalog);
    expect(h.rpc).toHaveBeenCalledTimes(2);
  });

  it("成功结果复用缓存，force 时重新读取", async () => {
    const first = [
      { provider: "p", provider_name: "P", models: [], fetched_at: 1, source: "builtin" },
    ];
    const second = [
      { provider: "q", provider_name: "Q", models: [], fetched_at: 2, source: "remote" },
    ];
    h.rpc.mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    const { modelsCatalog } = await import("./models");

    await expect(modelsCatalog(true)).resolves.toEqual(first);
    await expect(modelsCatalog()).resolves.toEqual(first);
    await expect(modelsCatalog(true)).resolves.toEqual(second);
    expect(h.rpc).toHaveBeenCalledTimes(2);
  });
});
