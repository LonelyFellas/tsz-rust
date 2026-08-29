# 内置词典词形与发音覆盖需求评估

## 背景与目标

2026-08-28 本地真实检测稳定复现：`child` 的内置词典状态为 `matched`，但只返回
`noun/base=child`，没有复数 `children`，且 `pronunciations=[]`。响应真实标记
`coverage.forms=partial`、`coverage.pronunciations=missing`。

同一路径抽查 `color`、`run`、`can't` 也全部只有 base 且发音数为 0；
`state-of-the-art` 为 `not_found`。当前 UI 只突出“已匹配”，没有显著呈现覆盖缺口。

目标是让内置词典在有可追溯 Kaikki 证据时提供安全的常用词形与字典 IPA，同时让管理员明确
知道哪些分类只是部分覆盖或完全缺失。不得用拼写规则、语言模型或现有智能词库内容伪造词典证据。

## 目标端

- 后端：`tsz-rust` 的 dictionary schema、Kaikki 内容导入器、V3 detection/create。
- 前端：`tsz` Admin 的统一 Step 1 检测结果展示。
- 数据：当前 Kaikki English Wiktionary 版本对应的完整 JSONL，按可审计流程重新导入。

## 用户故事 / 使用场景

- 管理员检测 `child` 时，若当前 Kaikki 记录包含复数和 IPA，可看到 `children` 及字典音标建议。
- 管理员创建新草稿后，Step 2 自动带入有来源的词形和字典音标，不需要重复手工录入。
- Kaikki 只提供部分信息时，管理员仍可创建草稿，但 Step 1 明确显示“词形部分覆盖”“发音缺失”。
- Kaikki 标签无法安全映射时，系统忽略该项并保持 `partial/missing`，不猜测、不报内部错误。
- 已存在的智能词库草稿不因数据集升级被后台静默改写。

## 功能范围

### 本次范围内

- 完整 Kaikki 内容导入保留每条记录的 `forms` 与 `sounds` 原始数组及来源信息。
- 把有确定标签的常用词形映射为现有 V3 `SuggestedConcreteFormV3`：
  `plural`、`third_person_singular`、`present_participle`、`past_tense`、
  `past_participle`、`comparative`、`superlative`。
- 把非空 Kaikki `sounds[].ipa` 映射为字典音标；保留明确的 common/UK/US 证据。
- 继续复用现有 V3 suggested forms、coverage、provenance 和 create materialization 契约。
- 根据真实映射结果计算 coverage：
  - 有可用派生词形仍标记 `forms=partial`，因为 Kaikki 不保证形态穷尽；
  - 有至少一条可用 IPA 时标记 `pronunciations=partial`，否则 `missing`；
  - 只有实际提供证据的分类才写 provenance。
- Admin Step 1 显示 forms/pronunciations 的 `complete/partial/missing` 中文状态。
- 提供可校验、可审计、事务化的数据重导路径。

### 明确不在范围内

- 不生成或猜测 `actual_pron`；它仍由管理员按平台音标体系录入。
- 不在本次补 meanings、examples、frequency 或 CEFR。
- 不把智能词库已有词条的内容复制为内置词典证据。
- 不自动修改、重存或发布已有草稿/已发布词条。
- 不支持无法稳定映射到平台 form type 的任意 Kaikki 标签。
- 不把 coverage 伪装为 `complete`。

## 约束与边界

- Kaikki/Wiktextract 原始 `forms`、`sounds` 字段和 tag 是唯一数据来源；内容来源版本、输入 SHA-256、
  来源 URL 和解析规则版本必须独立于轻量词头版本可追溯。
- 未知、冲突或仅单侧地区证据必须 fail soft：忽略该候选并保留 partial，不得降级成 common。
- 相同 `(pos, form_type, dialect, spelling, ipa)` 必须确定性去重，输出顺序固定。
- 一个来源 form 同时表达 simple past 与 past participle 时，可以生成两个不同 form type 的建议，
  但不得共享或复用业务 UUID。
- 数据重导必须在单事务内替换当前内容派生表；失败时旧数据继续可用。
- 运行时检测在内容数据尚未升级时必须保持现有安全降级，不得因新列为空返回 500。
- 数据库重导、数据集激活、提交、推送、部署仍是独立授权门。

## 验收标准

- [x] 使用包含 forms/sounds 的受控 Kaikki fixture 导入后，`child` 检测返回 noun base 与
      `plural=children`，且来源/provenance 指向同一数据集版本。
- [x] fixture 提供 `sounds[].ipa` 时，base 建议至少包含一条 `dict_phonetic`，不生成
      `actual_pron`。
- [x] `child` create 后 Step 2 包含稳定的新 UUID、base、plural 和字典 IPA；不复制任何旧词条 UUID。
- [x] `color`、`run`、`can't` 的安全可映射词形按来源返回；未知标签被忽略且顺序确定。
- [x] 无 forms/sounds 的 matched 记录继续返回 base 骨架、forms partial、pronunciations missing。
- [x] not_found 与 unavailable 分支不回归。
- [x] 已存在草稿数据在导入和检测升级后不被扫描、重存或修改。
- [x] Admin Step 1 明确展示“词形部分覆盖 / 发音部分覆盖或缺失”。
- [x] 导入 validate-only、备份、事务导入、后端测试、OpenAPI/前端契约、Admin 测试和真实浏览器验收全部通过。

## 开放问题（推荐答案）

1. **是否生成实际发音？** 推荐不生成，只导入字典 IPA。
2. **是否一次覆盖全部 Kaikki form tag？** 推荐第一期只做七类确定映射，其他保留审计计数。
3. **coverage 是否升级为 complete？** 推荐第一期 forms/pronunciations 最高为 partial。
4. **是否自动回填已有草稿？** 推荐不回填；只影响新检测和新创建。
5. **是否同时补 Step 1 覆盖提示？** 推荐一起做，否则 matched 仍可能被误读为完整。
