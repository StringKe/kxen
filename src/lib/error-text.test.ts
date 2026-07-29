// formatError 实测：JSON 提取（包裹/顶层形态）+ 非 JSON 单行截断兜底。
import { describe, expect, it } from "vitest";
import { formatError } from "./error-text";

describe("formatError", () => {
  it("提取 error 包裹形态的 type/message，保留前缀", () => {
    const raw =
      'anthropic HTTP 401 Unauthorized: {"error":{"type":"authentication_error","message":"invalid x-api-key"}}';
    expect(formatError(raw)).toBe(
      "anthropic HTTP 401 Unauthorized: authentication_error: invalid x-api-key",
    );
  });

  it("提取顶层 type/message 形态", () => {
    const raw = 'openai HTTP 429: {"type":"rate_limit","message":"slow down"}';
    expect(formatError(raw)).toBe("openai HTTP 429: rate_limit: slow down");
  });

  it("只有 message 无 type 时不带类型段", () => {
    const raw = 'HTTP 500: {"error":{"message":"boom"}}';
    expect(formatError(raw)).toBe("HTTP 500: boom");
  });

  it("无前缀纯 JSON 也能提取", () => {
    const raw = '{"error":{"type":"server_error","message":"overloaded"}}';
    expect(formatError(raw)).toBe("server_error: overloaded");
  });

  it("非 JSON 文本原样单行返回", () => {
    expect(formatError("connection reset by peer")).toBe("connection reset by peer");
  });

  it("多行文本折叠为单行", () => {
    expect(formatError("line one\nline two\n  line three")).toBe("line one line two line three");
  });

  it("超长文本截断到 120 字符（含省略号）", () => {
    const raw = "x".repeat(200);
    const out = formatError(raw);
    expect(out.length).toBe(120);
    expect(out.endsWith("…")).toBe(true);
  });

  it("截断的 JSON 尾巴落兜底截断", () => {
    const raw = 'anthropic HTTP 401: {"error":{"type":"authentication_er';
    const out = formatError(raw);
    expect(out.length).toBeLessThanOrEqual(120);
    expect(out).toContain("anthropic HTTP 401");
  });

  it("JSON 无 message 字段落兜底", () => {
    const raw = 'HTTP 400: {"error":{"type":"bad_request"}}';
    expect(formatError(raw)).toBe('HTTP 400: {"error":{"type":"bad_request"}}');
  });

  it("空串返回空串", () => {
    expect(formatError("")).toBe("");
  });

  it("Error 与非字符串拒绝值统一转为可展示文本", () => {
    expect(formatError(new Error("permission denied"))).toBe("permission denied");
    expect(formatError({ code: 7 })).toBe("[object Object]");
  });
});
