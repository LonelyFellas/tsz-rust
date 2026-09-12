# 例句译文带上语言，为多语言译文留出结构

对应禅道 TASK#28「例句译文的语言，应改为可选项，而不是默认文本」。
子任务 TASK#32（新建例句默认摆出初/中/高/高四个录入位）已在前端单独落地，与本文无关。

## 背景

一条例句的译文现在被当成「汉语译文」处理：wire 上的 `WordSentenceTranslationV3`
只有 `id` / `band` / `content`，语言不进契约；库里 `lexicon.text_variants` 虽然有
`language` 列，但译文行由后端写死 `zh`，`field_role` 也写成 `zh_translation_<band>`。

产品预期未来出现英语译文、法语译文等多语种译文。现阶段仍然只开放汉语一种，
但数据结构必须按多语言组织，避免第三期「多语言引入」时再做一次破坏性契约变更。

## 范围

- 例句译文 `WordSentenceTranslationV3` 的 wire 结构、读写链路与发布快照。
- 前端 admin 的译文录入区：语言从写死文案改为选项，当前只有汉语一项。

范围外：

- 释义、语法结构、英文例句正文的语言处理，本次不动。
- 真正引入第二种语言的产品流程（选项开放、学习端消费、导入导出），留给第三期。
- 译文档位 `band` 的取值与语义，不随本次变化。

## 需求

1. 译文在 wire 上带 `language`，取值现阶段只有 `zh`，结构上允许将来扩充。
2. 后端读写以 wire 的 `language` 为准，不再写死 `zh`；库里既有译文行的语言保持 `zh`。
3. 请求缺省 `language` 时按 `zh` 处理，保证旧版前端仍可写入。
4. 发布快照同样记录译文语言，已发布内容的语言不因本次改动而改变。
5. 前端译文录入区把「汉语译文」从写死文案改为语言选项，默认选中汉语，
   当前下拉只有汉语一项。新建例句与新增译文行都带上所选语言。

## 约束

- 后端 `WordSentenceTranslationV3` 是 `#[serde(deny_unknown_fields)]`，
  前端 V3 运行时校验是 `additionalProperties: false`，两侧都拒绝未声明字段。
- 前端的 `admin-word-v3.runtime-schema.json` 由 `sync:openapi` 从后端
  `docs/openapi.json` 生成，字段必填与否直接继承后端 spec。
- 因此响应里的 `language` 必须是非必填字段，让前端新版在后端发版前后都能解析。
- 同句同档多条译文已由迁移 `20260907180000_allow_multiple_sentence_translations` 放开，
  本次不得收窄该能力。

## 发布顺序

**分三次部署：前端放宽校验 → 后端上字段 → 前端开录入。**

两侧都拒绝未声明字段，所以新字段在两个方向上各有一次不兼容窗口，不能靠一次同批发布绕过。

1. **前端**先上一版只更新契约的构建：`sync:openapi` 后 `language` 进入
   `admin-word-v3.runtime-schema.json` 且非必填，请求侧仍然不发该字段。
   此时旧后端不返回 `language`，非必填校验照常通过。
2. **后端**再上线：响应固定带 `language`，请求缺省时按 `zh` 处理。
   此时前端已经认得这个字段，不会拒收响应；前端也还没开始发它，不会撞 `deny_unknown_fields`。
3. **前端**最后上线语言选择器，请求开始带 `language`。

第 3 步不能提前到第 1 步：后端 `WordSentenceTranslationV3` 带
`#[serde(deny_unknown_fields)]`，字段未落地前收到 `language` 会直接 422。

## 验收

后端：

```
cargo test --locked --all-features --lib sentence_translation_language
```

前端：

```
pnpm --filter @tsz/api-client test && pnpm --filter @tsz/admin test
```

人工验收：在 admin 新建例句，语言选择器默认显示汉语，保存后重新打开语言保持汉语；
已有词条的历史译文打开后语言同样显示汉语，不出现空值或未识别语言。
