//! REGISTRY 国内 API key 条目（拼接顺序即设置页展示顺序，由 mod.rs 引用）。

use super::seeds;
use super::spec::{AuthKind, Protocol, ProviderSpec, RegionSpec};

use AuthKind::ApiKey;
use Protocol::OpenAiCompat;

const CN: &str = "中国版";
const INTL: &str = "国际版";
const GL: &str = "全球";

pub const KIMI: ProviderSpec = ProviderSpec {
    key: "kimi",
    display: "Kimi",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://api.moonshot.cn/v1" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://api.moonshot.ai/v1" },
    ],
    models_endpoint: true,
    default_model: "kimi-k2.5",
    doc_url: "https://platform.moonshot.cn/docs",
    models_dev: Some("moonshotai"),
    static_models: seeds::KIMI,
};

pub const ZHIPU: ProviderSpec = ProviderSpec {
    key: "zhipu",
    display: "智谱 GLM",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://open.bigmodel.cn/api/paas/v4" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://api.z.ai/api/paas/v4" },
    ],
    models_endpoint: true,
    default_model: "glm-4.6",
    doc_url: "https://docs.bigmodel.cn",
    models_dev: Some("zhipuai"),
    static_models: seeds::ZHIPU,
};

// 智谱 Coding Plan 专属端点：套餐 key 与 PAYG key 不通用，独立条目避免用户混用
pub const ZHIPU_CODING: ProviderSpec = ProviderSpec {
    key: "zhipu-coding",
    display: "智谱 Coding Plan",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://open.bigmodel.cn/api/coding/paas/v4" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://api.z.ai/api/coding/paas/v4" },
    ],
    models_endpoint: true,
    default_model: "glm-5.2",
    doc_url: "https://docs.z.ai",
    models_dev: Some("zai-coding-plan"),
    static_models: seeds::ZHIPU_CODING,
};

pub const QWEN: ProviderSpec = ProviderSpec {
    key: "qwen",
    display: "通义千问",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1" },
    ],
    models_endpoint: true,
    default_model: "qwen3-max",
    doc_url: "https://help.aliyun.com/zh/model-studio",
    models_dev: Some("alibaba-cn"),
    static_models: seeds::QWEN,
};

// 百炼 Coding Plan 专属端点：套餐 key 与普通 dashscope key 不通用
pub const QWEN_CODING: ProviderSpec = ProviderSpec {
    key: "qwen-coding",
    display: "百炼 Coding Plan",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://coding.dashscope.aliyuncs.com/v1" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://coding-intl.dashscope.aliyuncs.com/v1" },
    ],
    models_endpoint: true,
    default_model: "qwen3-coder-plus",
    doc_url: "https://help.aliyun.com/zh/model-studio",
    models_dev: None,
    static_models: seeds::QWEN_CODING,
};

// models.dev 的 api 字段给的是 Anthropic 协议端点（/anthropic/v1，npm @ai-sdk/anthropic）；
// 这里用 MiniMax 官方文档的 OpenAI 兼容端点（同 host root 的 /v1），与仓库的 OpenAI 兼容薄层对齐
pub const MINIMAX: ProviderSpec = ProviderSpec {
    key: "minimax",
    display: "MiniMax",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://api.minimaxi.com/v1" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://api.minimax.io/v1" },
    ],
    models_endpoint: true,
    default_model: "MiniMax-M2.5",
    doc_url: "https://platform.minimaxi.com/document",
    models_dev: Some("minimax"),
    static_models: seeds::MINIMAX,
};

pub const SILICONFLOW: ProviderSpec = ProviderSpec {
    key: "siliconflow",
    display: "硅基流动",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://api.siliconflow.cn/v1" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://api.siliconflow.com/v1" },
    ],
    models_endpoint: true,
    default_model: "deepseek-ai/DeepSeek-V3.2",
    doc_url: "https://docs.siliconflow.cn",
    models_dev: Some("siliconflow-cn"),
    static_models: seeds::SILICONFLOW,
};

pub const STEPFUN: ProviderSpec = ProviderSpec {
    key: "stepfun",
    display: "阶跃星辰",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://api.stepfun.com/v1" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://api.stepfun.ai/v1" },
    ],
    models_endpoint: true,
    default_model: "step-3.5-flash",
    doc_url: "https://platform.stepfun.com/docs",
    models_dev: Some("stepfun"),
    static_models: seeds::STEPFUN,
};

// 阶跃 Step Plan 套餐专属端点：普通 /v1 不计套餐额度，独立目录
pub const STEPFUN_PLAN: ProviderSpec = ProviderSpec {
    key: "stepfun-plan",
    display: "阶跃 Step Plan",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[
        RegionSpec { key: "cn", display: CN, base_url: "https://api.stepfun.com/step_plan/v1" },
        RegionSpec { key: "intl", display: INTL, base_url: "https://api.stepfun.ai/step_plan/v1" },
    ],
    models_endpoint: true,
    default_model: "step-3.5-flash",
    doc_url: "https://platform.stepfun.com/docs",
    models_dev: Some("stepfun-step-plan"),
    static_models: seeds::STEPFUN_PLAN,
};

pub const DOUBAO: ProviderSpec = ProviderSpec {
    key: "doubao",
    display: "豆包",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://ark.cn-beijing.volces.com/api/v3" }],
    models_endpoint: true,
    default_model: "doubao-seed-1-6-250615",
    doc_url: "https://www.volcengine.com/docs/82379",
    models_dev: None,
    static_models: seeds::DOUBAO,
};

// 方舟 Coding Plan 专属端点：独立模型目录（ark-code-latest），旧 doubao 模型 id 会 400；
// 仅单区域（北京），套餐 key 与普通方舟 key 不通用
pub const DOUBAO_CODING: ProviderSpec = ProviderSpec {
    key: "doubao-coding",
    display: "豆包 Coding Plan",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://ark.cn-beijing.volces.com/api/coding/v3" }],
    models_endpoint: false,
    default_model: "ark-code-latest",
    doc_url: "https://www.volcengine.com/docs/82379",
    models_dev: None,
    static_models: seeds::DOUBAO_CODING,
};

pub const YI: ProviderSpec = ProviderSpec {
    key: "yi",
    display: "零一万物",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.lingyiwanwu.com/v1" }],
    models_endpoint: true,
    default_model: "yi-lightning",
    doc_url: "https://platform.lingyiwanwu.com/docs",
    models_dev: None,
    static_models: seeds::YI,
};

pub const HUNYUAN: ProviderSpec = ProviderSpec {
    key: "hunyuan",
    display: "腾讯混元",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.hunyuan.cloud.tencent.com/v1" }],
    models_endpoint: true,
    default_model: "hunyuan-turbos-latest",
    doc_url: "https://cloud.tencent.com/document/product/1729",
    models_dev: None,
    static_models: seeds::HUNYUAN,
};

// 腾讯 Coding Plan 专属端点（lkeap 域名）：套餐 key 与普通混元 key 不通用
pub const HUNYUAN_CODING: ProviderSpec = ProviderSpec {
    key: "hunyuan-coding",
    display: "腾讯 Coding Plan",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.lkeap.cloud.tencent.com/coding/v3" }],
    models_endpoint: true,
    default_model: "tc-code-latest",
    doc_url: "https://cloud.tencent.com/document/product/1729",
    models_dev: Some("tencent-coding-plan"),
    static_models: seeds::HUNYUAN_CODING,
};

pub const QIANFAN: ProviderSpec = ProviderSpec {
    key: "qianfan",
    display: "百度千帆",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://qianfan.baidubce.com/v2" }],
    models_endpoint: true,
    default_model: "ernie-4.5-turbo-128k",
    doc_url: "https://cloud.baidu.com/doc/WENXINWORKSHOP",
    models_dev: None,
    static_models: seeds::QIANFAN,
};

// 千帆 Coding Plan 专属路径 /v2/coding：OpenAI 兼容，推荐 qianfan-code-latest；
// 套餐 key 与普通千帆 key 不通用
pub const QIANFAN_CODING: ProviderSpec = ProviderSpec {
    key: "qianfan-coding",
    display: "千帆 Coding Plan",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://qianfan.baidubce.com/v2/coding" }],
    models_endpoint: false,
    default_model: "qianfan-code-latest",
    doc_url: "https://cloud.baidu.com/doc/qianfan",
    models_dev: None,
    static_models: seeds::QIANFAN_CODING,
};
