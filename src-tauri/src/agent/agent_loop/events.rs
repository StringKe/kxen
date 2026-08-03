//! loop 事件与运行结果类型。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    /// arguments 是精确调用参数（转录落盘用）；summary 只是一行摘要（UI 头行）
    ToolCall {
        name: String,
        summary: String,
        arguments: String,
    },
    /// output 是完整结果（转录落盘用）；summary 保留给 UI 头行
    ToolResult {
        name: String,
        summary: String,
        output: String,
    },
    /// auto-compact 发生：摘要供会话落检查点（不上行前端）
    Compacted {
        summary: String,
    },
    /// workflow phase 进度；index/total/workflow_name 仅当脚本声明了 meta 时带上（None 时不上行，保持旧 payload 形状）
    Phase {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workflow_name: Option<String>,
    },
    Done {
        turns: u32,
        stats: Option<RunStats>,
    },
    Aborted,
    Error {
        message: String,
    },
}

/// 单轮运行统计（TTFT / 耗时 / tok/s / tokens）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RunStats {
    pub ttft_ms: u64,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub unmetered_calls: u64,
    pub usage_complete: bool,
    /// 最近一次请求的 input tokens（ctx 当前水位；累计 input 不代表窗口占用）
    pub last_input_tokens: u64,
    pub tokens_per_sec: u64,
}

#[derive(Debug)]
pub struct AgentOutcome {
    pub final_text: String,
    pub turns: u32,
    pub aborted: bool,
    pub stats: Option<RunStats>,
    pub terminal: AgentEvent,
    /// 本 run 真正开始过请求的模型。本地预检、admission、no-op 失败保持 None。
    pub provider_model: Option<crate::llm::ModelRef>,
}
