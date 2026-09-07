---
name: publish-word
description: 按用户给定词表调用 tsz-rust 的原生 admin API 脚本创建并发布 V3 词条。用于明确的词条入库或批量发布请求；不用于查询、只整理词表或修改发布功能代码。
---

# 发布词条

使用 [publish_words.py](../../../ops/lexicon-publish/publish_words.py)，输入字段与能力见 [README](../../../ops/lexicon-publish/README.md) 和 [示例](../../../ops/lexicon-publish/example-words.json)。
流程为 detect → create → forms → meanings → validate → publish。脚本没有去重，也没有整批事务回滚。

## 目标与输入

- 未指定环境时采用本地，并**显式传入** `--base-url http://127.0.0.1:8383`，覆盖脚本可能读取的 TSZ_BASE_URL。
- 用户明确要求写入 tshb-test 已构成该环境的发布授权，先说明目标和词表再执行，不重复确认。同一词表授权不包含额外词条、其他环境或删除既有数据。
- 未明确远端写入意图时，先把词表与具体目标准备好再请求决定，不能仅凭环境变量向远端写入。
- 密码与 token 只从 TSZ_ADMIN_PHONE、TSZ_ADMIN_PASSWORD、TSZ_ADMIN_TOKEN 或脚本私有会话缓存读取；不放在命令参数、词表文件或输出里。
- 缺失的释义需要用户提供。词性、等级、词频和音标等默认值必须在发布前说明；音标占位不是真实读音。
- 写前检查输入重复，并通过可用只读 API 核对已有词条。已经存在或结果不明的词条先核实，不能用再次 create 试探。

## 执行

从仓库根用完整路径，例如：

```bash
python3 ops/lexicon-publish/publish_words.py --base-url http://127.0.0.1:8383 'harbour:港口' 'apple:苹果'
```

多词性、义项、英美拼写或派生词形使用临时目录 JSON 加 `--file`；不在仓库留下临时词表。
先核对目标 health 和脚本前置条件。服务/凭据缺失时报告具体阻塞，不通过改业务代码、配置或校验规则让发布勉强成功。
sub_pos 必须匹配基本词性；写前依据目录和输入规范核对，不靠反复执行发布获取错误提示。

## 失败与重试

- 保存每条成功结果、entry id、失败步骤与可见 code。创建成功后后续失败可能留下草稿，不能将失败等同于「没有写入」。
- 超时、断连或部分失败时先只读确认服务端结果，不重跑整批。脚本不能续传既有草稿时，报告 id/状态并提出最小处理动作。
- 401 依据实际 code 区分会话、凭据和验证码问题；不把所有 401 都归为短信冷却，也不反复触发登录。
- surface match 确认仍失败或 field_issues 时报告具体字段和阶段；补完内容也须先核对已有草稿，避免重复 create。

交付成功词条及 id、默认值、失败/不确定项和留下的草稿状态；已发布内容不会自动回滚。
