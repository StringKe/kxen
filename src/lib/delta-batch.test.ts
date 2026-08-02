import { afterEach, describe, expect, it, vi } from "vitest";
import { createDeltaBatcher } from "./delta-batch";

afterEach(() => {
  vi.useRealTimers();
});

describe("createDeltaBatcher", () => {
  it("coalesces both fields into one scheduled flush", () => {
    vi.useFakeTimers();
    const append = vi.fn();
    const batcher = createDeltaBatcher(append, 50);

    batcher.push("content", "a");
    batcher.push("content", "b");
    batcher.push("reasoning", "r");
    expect(append).not.toHaveBeenCalled();

    vi.advanceTimersByTime(50);
    expect(append.mock.calls).toEqual([
      ["content", "ab"],
      ["reasoning", "r"],
    ]);
  });

  it("flushNow drains pending content and is inert when empty", () => {
    vi.useFakeTimers();
    const append = vi.fn();
    const batcher = createDeltaBatcher(append);

    batcher.flushNow();
    batcher.push("reasoning", "thought");
    batcher.flushNow();
    batcher.flushNow();

    expect(append).toHaveBeenCalledOnce();
    expect(append).toHaveBeenCalledWith("reasoning", "thought");
  });

  it("discard cancels both the pending payload and timer", () => {
    vi.useFakeTimers();
    const append = vi.fn();
    const batcher = createDeltaBatcher(append, 50);

    batcher.push("content", "stale");
    batcher.discard();
    vi.advanceTimersByTime(50);
    expect(append).not.toHaveBeenCalled();

    batcher.push("content", "fresh");
    vi.advanceTimersByTime(50);
    expect(append).toHaveBeenCalledOnce();
    expect(append).toHaveBeenCalledWith("content", "fresh");
  });

  it("flushNow clears the old timer so it cannot flush a later batch early", () => {
    vi.useFakeTimers();
    const append = vi.fn();
    const batcher = createDeltaBatcher(append, 50);

    batcher.push("content", "first");
    vi.advanceTimersByTime(20);
    batcher.flushNow();
    batcher.push("content", "second");
    vi.advanceTimersByTime(30);
    expect(append).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(20);
    expect(append).toHaveBeenLastCalledWith("content", "second");
  });
});
