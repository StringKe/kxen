export const homeTitle = "kxen";
export const homeDescription = "面向复杂软件工程任务的 macOS 原生 Coding Agent Harness。";

export const homeBody = `kxen 是一个面向复杂软件工程任务的 macOS 原生 Coding Agent Harness。它让用户在一个本地应用中组织工作目录、会话、模型、目标和 Agent 执行过程。

当前版本是开发预览。公开发行、签名下载和自动更新尚未开放。

## 产品文档

- [产品概览](https://kxen.ai/overview/)
- [开始使用](https://kxen.ai/getting-started/)
- [Workspace](https://kxen.ai/workspace/workspace/)
- [模型与 Provider](https://kxen.ai/models/)
- [Agent 与任务](https://kxen.ai/agent/)
- [知识与定制](https://kxen.ai/knowledge/)
- [集成能力](https://kxen.ai/integrations/)
- [恢复与隔离](https://kxen.ai/recovery/)
- [参考手册](https://kxen.ai/reference/)
- [核心概念](https://kxen.ai/concepts/)

## 稳定边界

- 平台限定为 macOS 14 及以上版本和 Apple Silicon。
- 应用形态是 Tauri 2 桌面应用，不提供 CLI、TUI 或公开 HTTP API。
- Rust 后端拥有运行状态，SolidJS 前端负责交互和呈现。
- 所有模型调用进入统一资源管理层。
- 高风险工具调用在执行层统一审批或拒绝。
- 文件删除进入废纸篓，不直接执行不可恢复删除。`;

export const homeSource = `---
title: ${homeTitle}
description: ${homeDescription}
---

${homeBody}
`;
