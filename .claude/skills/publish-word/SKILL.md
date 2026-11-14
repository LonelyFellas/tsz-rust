---
name: publish-word
description: 发布词条的 Claude 兼容入口，读取本仓 .agents 流程；仅用于明确的词条入库或发布请求。
---

# 发布词条入口

读取 [项目 publish-word](../../../.agents/skills/publish-word/SKILL.md)，沿用目标环境、词表授权、凭据保护、幂等核对和失败恢复约束，不重复执行。
