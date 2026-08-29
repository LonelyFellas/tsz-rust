# 内置词典词形与发音覆盖技术设计

## 方案概述

采用“原始证据持久化 → 纯函数安全映射 → 现有 V3 suggestion 契约 → 前端透明展示”的路线。

现有完整内容导入器已经读取整条 Kaikki JSONL，但写库时只保留 `senses`；运行时检测只读取
`dictionary.active_terms`，所以必然只能创建 base 骨架并把发音标为 missing。修复不新增检测 API，
而是扩展现有内容存储和 repository 查询，让 `detect_v3` 使用当前 active dataset 的 forms/sounds
证据构造已有 `SuggestedConcreteFormV3`。

不选择“按英语规则推导 children/runs/running”方案，因为 irregular form、地区差异、同形异义和
发音都无法由字符串安全推出；也不选择从智能词库复制，因为那会污染 builtin provenance 和稳定身份。

## 现状证据

- `dictionary.entry_contents` 当前只有 `senses JSONB`。
- `import_dictionary_content` 解析完整 payload，却只 INSERT `payload->'senses'`。
- `LexiconRepository::dictionary_term` 只返回 term/POS/region/provider。
- `detect_v3` 为每个 POS 硬构造一个 common base，pronunciations 固定为空，并固定返回
  forms partial / pronunciations missing。
- `materialize_v3_detection_forms` 已能把多 concrete form、多 pronunciation 安全物化为新 UUID，
  无需修改 create wire。

上游字段依据：

- [Kaikki raw data](https://kaikki.org/dictionary/rawdata.html)
- [Wiktextract English TypedDict：FormData / SoundData / WordData](https://github.com/tatuylonen/wiktextract/blob/master/src/wiktextract/extractor/en/type_utils.py)

## 代码影响范围

### 后端 tsz-rust

- `migrations/<new>_extend_dictionary_content_forms_sounds.*.sql`
  - `dictionary.entry_contents` 增加 `forms JSONB`、`sounds JSONB`，默认空数组并约束类型。
  - `dictionary.content_imports` 增加 `parser_version`，区分旧 senses-only 与新解析规则。
  - `dictionary.content_imports` 独立记录 `source_version`；轻量词头与完整内容版本不必伪装成相同。
- `src/bin/import_dictionary_content.rs`
  - 校验 forms/sounds 为缺省或数组；导入时保留两者。
  - 增加 validate-only 统计：记录数、含 forms/sounds 数、未知标签数、关键回归词命中。
  - 增加显式、事务化的内容替换模式；默认仍拒绝覆盖既有 import。
- `src/lexicon/model.rs`
  - 新增运行时词典内容记录类型。
- `src/lexicon/repository/dictionary.rs`
  - 按 active dataset + normalized term 读取 pos/forms/sounds/source。
- `src/lexicon/dictionary_suggestions.rs`（新增）
  - 纯函数解析、tag 映射、地区分类、去重、稳定排序及 coverage 统计。
- `src/lexicon/service/v3.rs`
  - `detect_v3` 合并轻量 term/POS 与完整 forms/sounds 证据。
  - 没有内容记录时保持现有 base-only 降级。
- `tests/lexicon_handler.rs`、importer/repository 单测
  - 增加 child、规则/不规则、多 POS、地区、未知标签和无内容回归。
- `docs/dictionary-content-import.md`、`docs/frontend-integration.md`
  - 更新导入、coverage 和运维步骤。

### 前端 tsz

- `apps/admin/src/features/dictionary/word-creation/UnifiedCreateEntryStep.tsx`
  - 在 matched 结果中展示 forms/pronunciations coverage，不改变创建 payload。
- 对应组件测试和产品测试矩阵。
- `@tsz/types` / `@tsz/api-client` wire 形状不变；后端重导 OpenAPI 后前端只做一致性确认。

## 数据模型

```sql
ALTER TABLE dictionary.entry_contents
  ADD COLUMN forms JSONB NOT NULL DEFAULT '[]'::jsonb
    CHECK (jsonb_typeof(forms) = 'array'),
  ADD COLUMN sounds JSONB NOT NULL DEFAULT '[]'::jsonb
    CHECK (jsonb_typeof(sounds) = 'array');

ALTER TABLE dictionary.content_imports
  ADD COLUMN parser_version TEXT NOT NULL DEFAULT 'senses-only-v1',
  ADD COLUMN source_version TEXT NOT NULL;
```

运行时不直接返回 raw JSON；raw 只在 dictionary schema 内保存。业务响应继续使用已有 typed V3 DTO。

## 安全映射规则

### 词形

只接受非空 `form` 和明确 tag 组合：

| Kaikki tags | V3 form_type |
| --- | --- |
| `plural` | `plural` |
| `present + singular + third-person`（`simple` 可有可无） | `third_person_singular` |
| `present + participle` | `present_participle` |
| `past` 且不含 `participle`（`simple` 可有可无） | `past_tense` |
| `past + participle` | `past_participle` |
| `comparative` | `comparative` |
| `superlative` | `superlative` |

- 一条记录同时命中两类时分别产生两条 typed suggestion。
- 与 base 同拼写不自动删除；同一完整业务键重复才去重。
- 带 `alternative/archaic/dialectal/nonstandard/obsolete/rare` 等低质量或非规范标签的 forms
  不进入建议，只计审计。
- 无地区 tag 的候选归 common；只有明确且成对的 UK/US 证据才生成 `uk_us`。
- 单侧地区、冲突地区、未知 tag 记录进入审计计数，不伪装成 common。

### 发音

- 只接受 trim 后非空的 `sounds[].ipa`。
- `sound.form` 缺失或等于 headword 时绑定 base；等于某个来源 form 时绑定该 form。
- 明确单侧 UK/US tag 分流；同一 IPA 同时标记 UK+US 时作为共享 common；无地区 tag 绑定
  common；其他地区或冲突 tag 忽略并计数。
- `dict_phonetic` 原样保留 IPA；`actual_pron` 不赋值；style 缺省 normal。
- 相同 form/dialect/IPA 确定性去重。

### coverage / provenance

- matched 且存在词典 term：forms 至少为 partial，第一期不返回 complete。
- 至少导入一条有效 IPA：pronunciations=partial；否则 missing。
- forms/pronunciations provenance 只在对应有效证据存在时写 content import 的 provider/source_version；
  顶层词头 provider 继续保留轻量 active dataset 的版本。
- meanings/examples/frequency 本次维持现状。

## 数据流

```text
Kaikki English JSONL
  -> import_dictionary_content validate-only
  -> entry_contents(senses, forms, sounds, source_key, parser_version manifest)
  -> dictionary_term + dictionary_contents(normalized_surface)
  -> map_dictionary_pos + dictionary_suggestions pure mapper
  -> DetectLexiconSurfaceResponseV3.suggested_forms / coverage / provenance
  -> existing detection snapshot
  -> create_v3 consumes snapshot once
  -> materialize_v3_detection_forms generates new UUIDs
  -> Admin Step 2 prefilled forms + dictionary IPA
```

## 导入与激活策略

1. 获取 Kaikki English-only JSONL，记录下载 URL、日期、SHA-256 和独立 source_version；允许内容版本
   晚于轻量词头版本，但两类 provenance 必须分别返回。
2. 先执行 validate-only，输出关键覆盖统计和 `child/color/run/can't` 样本摘要。
3. 对数据库做备份并记录 active dataset/content manifest。
4. 使用显式 `--replace-existing --parser-version forms-sounds-v1`；要求：
   - 目标 dataset 必须仍为 active；
   - 新旧输入 SHA/source 一致，或调用者提供新的 dataset version；
   - 删除旧 entry_contents、COPY 新内容、更新 manifest 在同一事务提交。
5. ANALYZE 后调用真实 detection；未达验收门则恢复备份/旧 binary。

数据重导属于独立运维授权。本功能代码完成不自动执行本地、测试或生产数据替换。

## 前端展示

Step 1 的 matched 卡增加紧凑状态：

- `forms=partial` → “词形：部分覆盖”
- `pronunciations=partial` → “发音：部分覆盖”
- `pronunciations=missing` → “发音：词典未提供”

不显示内部 enum、provider record key 或 detection ID；详细 provider/version 可继续留在诊断信息。

## 测试策略

### 导入器/纯函数

- child noun fixture：base + plural children + common IPA。
- run：同一 spelling 同时映射 past_tense/past_participle，去重但保留两个 form type。
- verb 完整规则 forms、comparative/superlative、多 POS。
- UK/US 成对、单侧、冲突、未知 tag、空字符串、重复记录。
- validate-only 不连接数据库；replace-existing 的 SHA/parser/version 门禁。

### Repository / HTTP / DB

- active/retired dataset 隔离。
- detection matched with/without content rows；coverage/provenance 准确。
- create 消费建议后新 UUID、顺序、group membership、pronunciation round-trip 正确。
- detection snapshot/idempotency/surface-match/旧草稿不回归。

### 前端

- matched partial/missing 的中文提示。
- not_found/unavailable、重复匹配和创建确认不回归。
- Step 2 对多 form、多 pronunciation 的预填矩阵真实显示。

### 真实验收

- 新数据导入前后对比 child/color/run/can't 原始 detection。
- child 新建测试草稿验证 children/IPA；不发布，验收后按用户授权决定是否删除。
- 浏览器控制台/网络无错误；保留代表性现场。

## 风险与回滚

- **上游 tag 漂移：**未知值只计数并降级 partial；parser_version 固化规则。
- **数据量与锁：**替换在事务内，但会扩大 WAL/磁盘；导入前必须估算、备份并安排窗口。
- **地区误标：**单侧或冲突证据不降级 common。
- **误覆盖人工数据：**仅影响 detection/create，不扫描或更新既有 entries。
- **回滚：**旧 binary 忽略新增列；数据层恢复备份或重新导入旧 parser manifest。down migration
  仅在确认没有新 binary 读取列后执行。

## 推荐评审结论

建议批准以下边界后再动工：

1. 后端 forms/sounds 导入与安全映射、前端 coverage 提示一起做；
2. 实际发音不生成，词形/IPA 均只来自 Kaikki；
3. 第一期只映射七类明确 form tag，coverage 最高 partial；
4. 不自动改已有词条；
5. 代码实现与数据重导分开授权，未取得原始 JSONL 与备份确认前不执行数据库替换。
