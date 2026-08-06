//! 静态兜底模型种子：models.dev 首次不可达时的最小可用集（live 快照到手后覆盖）。
//! 数值取自 models.dev api.json 快照；models.dev 未收录官方源的厂商（doubao/yi/hunyuan/qianfan/sambanova）
//! 只放官方文档最确定的条目，宁少勿错。

use super::spec::StaticModel;

const fn m(id: &'static str, name: &'static str, context: u64, reasoning: bool, attachment: bool) -> StaticModel {
    StaticModel { id, name, context, reasoning, attachment }
}

pub const ANTHROPIC: &[StaticModel] = &[
    m("claude-opus-4-8", "Claude Opus 4.8", 1_000_000, true, true),
    m("claude-sonnet-4-6", "Claude Sonnet 4.6", 1_000_000, true, true),
    m("claude-haiku-4-5", "Claude Haiku 4.5", 200_000, true, true),
];

pub const OPENAI: &[StaticModel] = &[
    m("gpt-5.4", "GPT-5.4", 1_050_000, true, true),
    m("gpt-5-codex", "GPT-5-Codex", 400_000, true, true),
    m("o3", "o3", 200_000, true, true),
];

pub const XAI: &[StaticModel] = &[
    m("grok-4.5", "Grok 4.5", 500_000, true, true),
    m("grok-4.3", "Grok 4.3", 1_000_000, true, true),
    m("grok-build-0.1", "Grok Build 0.1", 256_000, true, true),
];

pub const KIMI_CODING: &[StaticModel] =
    &[m("k3", "Kimi K3", 1_048_576, true, false), m("kimi-for-coding", "Kimi K2.7 Code", 262_144, true, true)];

pub const GITHUB_COPILOT: &[StaticModel] = &[
    m("gpt-4.1", "GPT-4.1 (Copilot)", 1_047_576, false, true),
    m("gpt-4o", "GPT-4o (Copilot)", 128_000, false, true),
    m("claude-sonnet-4", "Claude Sonnet 4 (Copilot)", 200_000, true, true),
];

pub const QWEN_OAUTH: &[StaticModel] = &[
    m("qwen3-coder-plus", "Qwen3 Coder Plus", 1_000_000, true, true),
    m("qwen3-coder-flash", "Qwen3 Coder Flash", 1_000_000, true, false),
];

pub const GOOGLE_OAUTH: &[StaticModel] = &[
    m("gemini-2.5-pro", "Gemini 2.5 Pro (订阅)", 1_048_576, true, true),
    m("gemini-2.5-flash", "Gemini 2.5 Flash (订阅)", 1_048_576, true, true),
    m("gemini-3-pro-preview", "Gemini 3 Pro Preview (订阅)", 1_048_576, true, true),
    m("gemini-3-flash-preview", "Gemini 3 Flash Preview (订阅)", 1_048_576, true, true),
];

pub const MINIMAX_OAUTH: &[StaticModel] = &[
    m("MiniMax-M2.7", "MiniMax M2.7", 1_000_000, true, false),
    m("MiniMax-M2.7-highspeed", "MiniMax M2.7 Highspeed", 1_000_000, true, false),
];

// Kiro 模型 id 以 9router providers/registry/kiro.js 实证为准（kiro.dev/docs/models）
pub const KIRO: &[StaticModel] = &[
    m("claude-sonnet-4.5", "Claude Sonnet 4.5 (Kiro)", 200_000, true, true),
    m("claude-sonnet-4", "Claude Sonnet 4 (Kiro)", 200_000, true, true),
    m("claude-opus-4.5", "Claude Opus 4.5 (Kiro)", 200_000, true, true),
];

pub const OPENROUTER: &[StaticModel] = &[
    m("anthropic/claude-opus-4.8", "Claude Opus 4.8 (OpenRouter)", 1_000_000, true, true),
    m("openai/gpt-5.4", "GPT-5.4 (OpenRouter)", 1_050_000, true, true),
    m("x-ai/grok-4.5", "Grok 4.5 (OpenRouter)", 500_000, true, true),
];

pub const OLLAMA: &[StaticModel] = &[
    m("llama3.3", "Llama 3.3 70B", 131_072, false, false),
    m("qwen3", "Qwen3 32B", 131_072, true, false),
    m("deepseek-r1", "DeepSeek R1", 131_072, true, false),
];

pub const DEEPSEEK: &[StaticModel] = &[
    m("deepseek-chat", "DeepSeek Chat", 1_000_000, false, true),
    m("deepseek-reasoner", "DeepSeek Reasoner", 1_000_000, true, true),
    m("deepseek-v4-pro", "DeepSeek V4 Pro", 1_000_000, true, false),
];

pub const MISTRAL: &[StaticModel] = &[
    m("mistral-large-latest", "Mistral Large (latest)", 262_144, false, true),
    m("mistral-medium-latest", "Mistral Medium (latest)", 262_144, true, true),
    m("codestral-latest", "Codestral (latest)", 256_000, false, false),
    m("magistral-medium-latest", "Magistral Medium (latest)", 128_000, true, false),
];

pub const GROQ: &[StaticModel] = &[
    m("llama-3.3-70b-versatile", "Llama 3.3 70B", 131_072, false, false),
    m("openai/gpt-oss-120b", "GPT OSS 120B", 131_072, true, false),
    m("qwen/qwen3-32b", "Qwen3-32B", 131_072, true, false),
    m("meta-llama/llama-4-scout-17b-16e-instruct", "Llama 4 Scout 17B 16E", 131_072, false, true),
];

pub const GOOGLE: &[StaticModel] = &[
    m("gemini-2.5-pro", "Gemini 2.5 Pro", 1_048_576, true, true),
    m("gemini-2.5-flash", "Gemini 2.5 Flash", 1_048_576, true, true),
    m("gemini-2.5-flash-lite", "Gemini 2.5 Flash-Lite", 1_048_576, true, true),
    m("gemini-2.0-flash", "Gemini 2.0 Flash", 1_048_576, false, true),
];

pub const TOGETHER: &[StaticModel] = &[
    m("meta-llama/Llama-3.3-70B-Instruct-Turbo", "Llama 3.3 70B (Together)", 131_072, false, false),
    m("deepseek-ai/DeepSeek-V3", "DeepSeek-V3 (Together)", 131_072, false, false),
    m("deepseek-ai/DeepSeek-R1", "DeepSeek-R1 (Together)", 163_839, true, false),
    m("Qwen/Qwen3-Coder-480B-A35B-Instruct-FP8", "Qwen3 Coder 480B (Together)", 262_144, false, false),
];

pub const KIMI: &[StaticModel] = &[
    m("kimi-k2.5", "Kimi K2.5", 262_144, true, false),
    m("kimi-k2-thinking", "Kimi K2 Thinking", 262_144, true, false),
    m("kimi-k3", "Kimi K3", 1_048_576, true, true),
];

pub const ZHIPU: &[StaticModel] = &[
    m("glm-4.7", "GLM-4.7", 204_800, true, false),
    m("glm-4.6", "GLM-4.6", 204_800, true, false),
    m("glm-4.5-air", "GLM-4.5-Air", 131_072, true, false),
];

pub const QWEN: &[StaticModel] = &[
    m("qwen3-max", "Qwen3 Max", 262_144, false, false),
    m("qwen-plus", "Qwen Plus", 1_000_000, true, false),
    m("qwen3-coder-plus", "Qwen3 Coder Plus", 1_048_576, false, false),
];

pub const MINIMAX: &[StaticModel] = &[
    m("MiniMax-M2.5", "MiniMax M2.5", 204_800, true, false),
    m("MiniMax-M2.7", "MiniMax M2.7", 204_800, true, false),
    m("MiniMax-M3", "MiniMax M3", 1_000_000, true, true),
];

pub const SILICONFLOW: &[StaticModel] = &[
    m("deepseek-ai/DeepSeek-V3.2", "DeepSeek V3.2 (SiliconFlow)", 164_000, true, false),
    m("deepseek-ai/DeepSeek-V3", "DeepSeek V3 (SiliconFlow)", 164_000, false, false),
    m("Qwen/Qwen3-8B", "Qwen3 8B (SiliconFlow)", 131_000, true, false),
];

pub const DOUBAO: &[StaticModel] = &[
    m("doubao-seed-1-6-250615", "Doubao Seed 1.6", 256_000, true, false),
    m("doubao-seed-1-6-flash-250828", "Doubao Seed 1.6 Flash", 256_000, true, false),
];

pub const STEPFUN: &[StaticModel] = &[
    m("step-3.5-flash", "Step 3.5 Flash", 256_000, true, false),
    m("step-3.7-flash", "Step 3.7 Flash", 256_000, true, true),
    m("step-2-16k", "Step 2 16K", 16_384, false, false),
];

pub const YI: &[StaticModel] = &[m("yi-lightning", "Yi Lightning", 16_384, false, false), m("yi-large", "Yi Large", 32_768, false, false)];

pub const HUNYUAN: &[StaticModel] =
    &[m("hunyuan-turbos-latest", "Hunyuan TurboS", 32_768, false, false), m("hunyuan-t1-latest", "Hunyuan T1", 32_768, true, false)];

pub const QIANFAN: &[StaticModel] = &[
    m("ernie-4.5-turbo-128k", "ERNIE 4.5 Turbo 128K", 128_000, false, false),
    m("ernie-x1-turbo-32k", "ERNIE X1 Turbo 32K", 32_768, true, false),
];

pub const FIREWORKS: &[StaticModel] = &[
    m("accounts/fireworks/models/gpt-oss-120b", "GPT OSS 120B (Fireworks)", 131_072, true, false),
    m("accounts/fireworks/models/deepseek-v4-pro", "DeepSeek V4 Pro (Fireworks)", 1_000_000, true, false),
    m("accounts/fireworks/models/glm-5p1", "GLM 5.1 (Fireworks)", 202_800, true, false),
];

pub const CEREBRAS: &[StaticModel] = &[
    m("gpt-oss-120b", "GPT OSS 120B (Cerebras)", 131_072, true, false),
    m("zai-glm-4.7", "GLM 4.7 (Cerebras)", 131_072, true, false),
    m("gemma-4-31b", "Gemma 4 31B (Cerebras)", 131_072, true, true),
];

pub const SAMBANOVA: &[StaticModel] = &[
    m("Meta-Llama-3.3-70B-Instruct", "Llama 3.3 70B (SambaNova)", 131_072, false, false),
    m("DeepSeek-R1", "DeepSeek R1 (SambaNova)", 32_768, true, false),
];

pub const PERPLEXITY: &[StaticModel] = &[
    m("sonar", "Sonar", 128_000, false, false),
    m("sonar-pro", "Sonar Pro", 200_000, false, true),
    m("sonar-reasoning-pro", "Sonar Reasoning Pro", 128_000, true, true),
];

pub const COHERE: &[StaticModel] = &[
    m("command-a-03-2025", "Command A", 256_000, false, false),
    m("command-a-reasoning-08-2025", "Command A Reasoning", 256_000, true, false),
    m("command-r-08-2024", "Command R", 128_000, false, false),
];

pub const GITHUB_MODELS: &[StaticModel] = &[
    m("openai/gpt-4.1-mini", "GPT-4.1 Mini (GitHub Models)", 128_000, false, false),
    m("openai/gpt-4o", "GPT-4o (GitHub Models)", 128_000, false, true),
    m("microsoft/phi-4", "Phi-4 (GitHub Models)", 16_000, true, false),
];

pub const NOVITA: &[StaticModel] = &[
    m("deepseek/deepseek-v3.1", "DeepSeek V3.1 (Novita)", 131_072, false, false),
    m("zai-org/glm-5", "GLM 5 (Novita)", 202_800, true, false),
    m("google/gemma-3-27b-it", "Gemma 3 27B (Novita)", 98_304, false, true),
];

pub const ZHIPU_CODING: &[StaticModel] = &[
    m("glm-5.2", "GLM-5.2 (Coding Plan)", 202_800, true, true),
    m("glm-5.2-highspeed", "GLM-5.2 Highspeed (Coding Plan)", 202_800, true, true),
    m("glm-5-turbo", "GLM-5 Turbo (Coding Plan)", 202_800, true, true),
    m("glm-4.7", "GLM-4.7 (Coding Plan)", 204_800, true, true),
];

pub const QWEN_CODING: &[StaticModel] = &[
    m("qwen3-coder-plus", "Qwen3 Coder Plus (Coding Plan)", 1_000_000, true, true),
    m("qwen3-coder-flash", "Qwen3 Coder Flash (Coding Plan)", 1_000_000, true, false),
    m("qwen3-max", "Qwen3 Max (Coding Plan)", 262_144, false, false),
];

// 方舟 Coding Plan 独立模型目录：ark-code-latest 为官方 Auto 调度入口
pub const DOUBAO_CODING: &[StaticModel] = &[
    m("ark-code-latest", "Ark Code (Coding Plan)", 262_144, true, true),
    m("doubao-seed-2.0-code", "Doubao Seed 2.0 Code", 262_144, true, true),
    m("glm-4.7", "GLM-4.7 (Ark Coding)", 204_800, true, true),
    m("deepseek-v3.2", "DeepSeek V3.2 (Ark Coding)", 164_000, true, false),
    m("kimi-k2.5", "Kimi K2.5 (Ark Coding)", 262_144, true, false),
];

pub const QIANFAN_CODING: &[StaticModel] = &[
    m("qianfan-code-latest", "Qianfan Code (Coding Plan)", 262_144, true, true),
    m("glm-4.7", "GLM-4.7 (千帆 Coding)", 204_800, true, true),
    m("deepseek-v3.2", "DeepSeek V3.2 (千帆 Coding)", 164_000, true, false),
    m("kimi-k2.5", "Kimi K2.5 (千帆 Coding)", 262_144, true, false),
];

pub const HUNYUAN_CODING: &[StaticModel] = &[
    m("tc-code-latest", "TC Code (Coding Plan)", 262_144, true, true),
    m("hunyuan-turbos", "Hunyuan TurboS (Coding Plan)", 262_144, true, false),
    m("hunyuan-2.0-thinking", "Hunyuan 2.0 Thinking (Coding Plan)", 262_144, true, false),
];

pub const STEPFUN_PLAN: &[StaticModel] = &[
    m("step-3.5-flash", "Step 3.5 Flash (Step Plan)", 256_000, true, false),
    m("step-3.7-flash", "Step 3.7 Flash (Step Plan)", 256_000, true, true),
];

pub const HUGGINGFACE: &[StaticModel] = &[
    m("deepseek-ai/DeepSeek-V3.2", "DeepSeek V3.2 (HF)", 164_000, true, false),
    m("zai-org/GLM-5", "GLM-5 (HF)", 202_800, true, false),
    m("meta-llama/Llama-3.3-70B-Instruct", "Llama 3.3 70B (HF)", 131_072, false, false),
];

pub const VERCEL: &[StaticModel] = &[
    m("anthropic/claude-sonnet-4.6", "Claude Sonnet 4.6 (Vercel)", 1_000_000, true, true),
    m("openai/gpt-5.4", "GPT-5.4 (Vercel)", 1_050_000, true, true),
    m("google/gemini-2.5-pro", "Gemini 2.5 Pro (Vercel)", 1_048_576, true, true),
];

pub const OLLAMA_CLOUD: &[StaticModel] = &[
    m("kimi-k2.5", "Kimi K2.5 (Ollama Cloud)", 262_144, true, false),
    m("glm-5.1", "GLM-5.1 (Ollama Cloud)", 202_800, true, false),
    m("deepseek-v4-flash", "DeepSeek V4 Flash (Ollama Cloud)", 1_000_000, true, false),
];
