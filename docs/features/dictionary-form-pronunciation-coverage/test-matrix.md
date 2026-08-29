# 内置词典词形与发音覆盖测试矩阵

| # | 层 | 场景 | 输入 / 前置 | 预期 | 优先级 |
| --- | --- | --- | --- | --- | --- |
| D1 | importer 单元 | forms/sounds 缺省 | Kaikki English 记录仅含 word/pos/senses | forms/sounds 保存为空数组；旧降级路径不报错 | P0 |
| D2 | importer 单元 | forms/sounds 合法 | child fixture 含 plural children 与 common IPA | validate-only 通过；导入 payload 保留原始数组 | P0 |
| D3 | importer 单元 | forms/sounds 非数组 | forms 或 sounds 为对象/字符串 | 明确拒绝并带行号，不写数据库 | P0 |
| D4 | mapper 单元 | noun plural | `forms=[{form:children,tags:[plural]}]` | 生成 `plural=children`，base 仍存在，顺序确定 | P0 |
| D5 | mapper 单元 | verb 规则词形 | third-person/present participle/past simple/past participle tags | 分别映射四类；同一 spelling 可对应 past 两类 | P0 |
| D6 | mapper 单元 | adjective/adverb | comparative、superlative tags | 映射对应 form type | P0 |
| D7 | mapper 单元 | 未知/冲突 tag | 未支持标签、空 form、单侧或冲突地区标签 | 忽略并计入审计，不猜测 common，不 panic | P0 |
| D8 | mapper 单元 | IPA 绑定 | base sound、带 form 的 derived sound、重复 IPA | 绑定正确 form/dialect并去重；只填 dict_phonetic | P0 |
| D9 | mapper 单元 | coverage/provenance | 有/无有效 forms 与 IPA | forms 保持 partial；IPA 有值为 partial、无值为 missing；provenance 与证据一致 | P0 |
| D10 | repository 集成 | active dataset 隔离 | active/retired 各有同词记录 | 只读取 active forms/sounds/source | P0 |
| D11 | HTTP 集成 | child detection | active fixture 含 child base/plural/IPA | matched；suggested_forms 包含 base+children；coverage/provenance 正确 | P0 |
| D12 | HTTP 集成 | 无完整内容的 matched | active_terms 命中，entry_contents 不存在或数组为空 | 保持 base-only、forms partial、pronunciations missing | P0 |
| D13 | HTTP 集成 | not_found/unavailable | 原有分支 | 响应、状态码和中文产品行为不回归 | P0 |
| D14 | create 集成 | 消费丰富 detection | child suggestion 后创建 | 新 UUID；base/plural/IPA 正确；actual_pron 空；不复制已有词条内容 | P0 |
| D15 | importer 集成 | 原子替换门禁 | 已有 content import，显式 replace/parser_version | 默认拒绝；满足 SHA/source/version 门禁时单事务替换；失败保留旧数据 | P0 |
| F1 | Admin 组件 | matched partial/missing | coverage forms partial、pronunciations missing | Step 1 显示“词形：部分覆盖”“发音：词典未提供” | P0 |
| F2 | Admin 组件 | matched pronunciation partial | 至少一条 IPA | 显示“发音：部分覆盖”，不显示内部 enum/UUID/状态码 | P0 |
| F3 | Admin 回归 | duplicate/confirmation | child 已有草稿并需确认 | 中文阻断与继续创建路径不变，不创建重复数据 | P0 |
| F4 | Admin Step 2 | 多 form/IPA 预填 | 新创建 child fixture | base、plural、字典 IPA 可见；实际发音仍空 | P0 |
| Q1 | QA 流程 | 最终列表路由核验 | 多条测试词条 | 一次 DOM/URL 清单 + 代表性目标点击；不再逐条全表回点两次 | P0 |
| Q2 | QA 泄漏回归 | 正常/阻断/异常路径 | 409/422、not_found、partial/missing | 不出现原始英文错误、内部 UUID 或 UI 状态码；中文产品文案保持 | P0 |
| Q3 | 真实数据 | child/color/run/can't | 完整 JSONL 已 validate-only、备份完成 | 导入前后 detection 对比符合来源；无应用请求错误或数据损坏 | 手测 |

## 执行边界

- D1–D15 与 F1–F4 先使用受控 fixture 跑绿，不依赖本机缺失的全量 Kaikki 文件。
- Q3 必须在原始 JSONL、SHA-256、来源定位和数据库备份齐备后单独授权执行。
- P0/P1 未发现、无泄漏属于验收通过项；本次只增加回归守护，不制造无意义代码变更。

## 2026-08-28 执行记录

- [x] D1–D9：forms/sounds 形状、child/run 映射、IPA、地区冲突、coverage/provenance 纯函数回归通过。
- [x] D11–D14：真实 HTTP fixture 验证 child detection/create 返回 base + children + IPA，existing-only POS 与旧降级不回归。
- [x] D15：replace-existing 默认拒绝、显式开关及 SHA/source identity 门禁通过；migration 在本地服务启动时成功应用。
- [x] F1–F3、Q1–Q2：Admin 中文 coverage 提示、无英文 enum/UUID/状态码泄漏、非冗余 QA 路径通过。
- [x] Q3：官方下载 English-only 2026-08-05 JSONL；过滤唯一空格词头后 1,487,638 条完整 validate-only；备份、事务导入、manifest/row count/SHA 核对通过。真实 child/color/run/can't 检测符合来源，state-of-the-art 保持 not_found。
