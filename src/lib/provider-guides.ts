// 各订阅厂商的凭证修复指引（设置页扳手按钮展开）；key 为 registry provider key。
export const GUIDES: Record<string, string[]> = {
  anthropic: [
    "1. 终端运行 `claude` 重新登录（订阅自动刷新到 Keychain）",
    "2. kxen 弹 keychain 读取请求时选「始终允许」",
    "3. 点「重新导入」",
  ],
  openai: ["1. 终端运行 `codex login` 重新登录", "2. 点「重新导入」"],
  xai: ["1. 终端运行 `grok` 触发登录刷新", "2. 点「重新导入」"],
  "kimi-for-coding": ["1. 终端运行 `kimi` 触发凭证刷新", "2. 点「重新导入」"],
  openrouter: [
    "1. 到 https://openrouter.ai/keys 创建 API Key",
    "2. 点「添加账号」选 openrouter 粘贴（kind 选 apikey）",
  ],
  ollama: [
    "1. 安装并运行 `ollama serve`",
    "2. `ollama pull llama3.3` 拉模型",
    "3. 无需凭证，直接选模型用",
  ],
  deepseek: [
    "1. 到 https://platform.deepseek.com 创建 API Key",
    "2. 点「添加账号」选 deepseek 粘贴（kind 选 API Key）",
  ],
  mistral: [
    "1. 到 https://console.mistral.ai 创建 API Key",
    "2. 点「添加账号」选 mistral 粘贴（kind 选 API Key）",
  ],
  groq: [
    "1. 到 https://console.groq.com/keys 创建 API Key",
    "2. 点「添加账号」选 groq 粘贴（kind 选 API Key）",
  ],
  google: [
    "1. 到 https://aistudio.google.com/apikey 创建 API Key",
    "2. 点「添加账号」选 google 粘贴（kind 选 API Key）",
  ],
  together: [
    "1. 到 https://api.together.xyz/settings/api-keys 创建 API Key",
    "2. 点「添加账号」选 together 粘贴（kind 选 API Key）",
  ],
};
