# Kaikki 词典正文导入

词条自动生成需要可追溯的英文释义正文。现有 `dictionary.active_terms` 仍只服务轻量词头检测；
完整 Kaikki JSONL 通过独立命令导入 `dictionary.entry_contents`，并绑定既有激活数据集版本。

## 来源与格式

权威来源为 Kaikki 的 English machine-readable dictionary。输入为一行一个对象的 JSONL
（可用 `.gz`），每条至少包含：

- `lang_code: "en"`
- `word`
- `pos`
- `senses` 数组；保留其中的 `glosses`、`raw_glosses`、`examples`、`tags`、`topics` 等原始字段
- 可选 `forms` / `sounds` 数组；导入器原样保留，用于 V3 安全词形与字典 IPA 建议

官方全量下载较大，导入前应单独下载到受控目录并记录下载页面、发布日期和文件校验和。
许可和署名遵循 Wiktionary/Kaikki 的 CC BY-SA 与 GFDL 要求。

## 只读校验

校验 JSONL 形状、记录数和 SHA-256，不连接数据库：

```bash
cargo run --release --bin import_dictionary_content -- \
  --dataset-version kaikki-en-2026-07-06-rules-v1 \
  --contents /path/to/kaikki.org-dictionary-English.jsonl.gz \
  --source-locator https://kaikki.org/dictionary/English/index.html \
  --source-version enwiktionary-2026-08-05 \
  --expected-records EXPECTED_COUNT \
  --validate-only
```

## 导入

确认 `.env` 的 `DATABASE_URL` 指向明确目标后，去掉 `--validate-only`。命令只允许向指定的
`active` 数据集导入一次；整个 COPY、数量核对、内容展开和 import manifest 写入同一事务，
失败会整体回滚。

```bash
cargo run --release --bin import_dictionary_content -- \
  --dataset-version kaikki-en-2026-07-06-rules-v1 \
  --contents /path/to/kaikki.org-dictionary-English.jsonl.gz \
  --source-locator https://kaikki.org/dictionary/English/index.html \
  --source-version enwiktionary-2026-08-05 \
  --expected-records EXPECTED_COUNT
```

既有 active dataset 已导入过内容时，默认仍拒绝覆盖。只有输入 SHA-256 与来源定位逐字一致，且
显式提供新的解析规则版本时，才允许在单事务内替换派生内容：

```bash
cargo run --release --bin import_dictionary_content -- \
  --dataset-version kaikki-en-2026-07-06-rules-v1 \
  --contents /path/to/kaikki.org-dictionary-English.jsonl.gz \
  --source-locator https://kaikki.org/dictionary/English/index.html \
  --source-version enwiktionary-2026-08-05 \
  --expected-records EXPECTED_COUNT \
  --parser-version forms-sounds-v1 \
  --replace-existing
```

执行替换前必须完成数据库备份、validate-only 和关键词样本核验；无备份不得执行。

`dictionary.content_imports` 保存输入 SHA-256、来源定位、内容来源版本、行数、解析规则版本和导入时间；
内容来源版本可晚于轻量词头数据集，运行时会分别写入 forms/pronunciations provenance，不冒充词头
provider version。每条内容使用
`kaikki:<normalized-term>:<pos>:<record-hash>` 作为稳定来源键。生成任务会把具体来源键与
数据集版本固化到任务快照，后续数据集切换不会改变已创建任务的依据。

## 核验

```sql
SELECT dataset.version, import.input_sha256, import.source_locator,
       import.source_version, import.record_count, import.parser_version
FROM dictionary.content_imports AS import
JOIN dictionary.datasets AS dataset ON dataset.id = import.dataset_id;

SELECT source_key, normalized_term, pos, jsonb_array_length(senses),
       jsonb_array_length(forms), jsonb_array_length(sounds), source_locator
FROM dictionary.entry_contents
WHERE normalized_term = 'bank'
ORDER BY pos, source_key;
```

未导入正文、词头未命中或目标词性没有正文时，worker 将该分区标记为
`missing/source_not_found`，不会让模型仅凭通用知识生成词义。

## 千问生成配置

导入正文后，显式选择千问并配置支持 JSON Schema 结构化输出的模型：

```dotenv
LEXICON_GENERATOR_PROVIDER=qwen
QWEN_LEXICON_API_KEY=replace-with-secret-manager-value
QWEN_LEXICON_MODEL=qwen3.8-max
# QWEN_LEXICON_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1
# QWEN_LEXICON_TIMEOUT_SECONDS=90
```

`API_KEY` 与 `MODEL` 必须同时存在；半配置或选择了 `qwen` 却缺少配置时应用启动失败。
任务 provenance 只记录 `provider=qwen`、模型名和提示版本，不保存密钥。请求只发送目标词条、
对应 Kaikki 来源正文和输出约束；worker 不会在千问失败时静默切换 provider 或生成模板内容。
