# 单元测试数据库环境疏漏审计

已向用户协调任务披露；已完成只读核对。没有尝试恢复、修改原库或删除原管理记录。
本文仅记录主机、端口和库名，不保存连接凭据。

## 发生的命令

以下命令先后运行两次，未显式设置 DATABASE_URL：

```sh
SQLX_OFFLINE=true CARGO_TARGET_DIR=/Users/darwish/.cargo-target cargo test --locked --all-features --lib --bins
```

日志分别为 `/tmp/entry_annotations_unit_gate.log`（OpenAPI断言失败）和
`/tmp/entry_annotations_unit_gate_final.log`（媒体类型修正后的成功复跑）。
三个内嵌 SQLx 测试在两次运行中均成功：

- `lexicon::service::v3::tests::retired_v3_variant_slot_rejects_a_replacement_uuid_before_writes`
- `lexicon::service::v3::tests::reverse_order_cross_entry_node_reuse_waits_then_returns_stable_validation`
- `lexicon::service::v3::tests::save_audit_includes_migration_batch_and_retry_is_action_idempotent`

进程未显式设置 DATABASE_URL，工作树 `.env` 指向 localhost:5433 的 `tsz_rust`。
SQLx 0.9.0 的 `sqlx-postgres/src/testing/mod.rs:93` 使用 dotenvy::var，导致旧库
被用作测试管理 master。这违反本任务显式隔离的要求。

## 已取得证据与边界

- SQLx源码 `testing/mod.rs:130–169`：在master确保 `_sqlx_test` 管理对象，按测试路径
  确定子库名，清理同名子库、登记记录并创建子库。
- 同文件 `:171–185`：测试池connect_opts显式 `.database(&db_name)`。
- 同文件 `:190–197`：成功后DROP子库并删除对应管理记录。
- 三个测试的fixture使用注入的PgPool，源码没有重新连接旧业务库。因此代码与成功日志
  支持：业务fixture/迁移运行于独立子库，master涉及测试管理SQL。
- 初次和事后只读枚举均有下列4个既存遗留库；事后管理表也仅这4条记录，3个单元测试的
  子库和管理记录未遗留。

| 既存库名（前后相同） | 事后test_path |
|---|---|
| `_sqlx_test_KM3UdZIyYYxK539RR5vNtM9USEwAI0ONPek0RoGKR5GBQIBrC79v` | `lexicon_handler::v3_sentence_translation_alias_is_idempotent_on_unchanged_meanings_save` |
| `_sqlx_test_PWxnyLfcn15JEfbdp3c6F7BDFHkp2Nw8yCJss7gLd0ishi08QbCQ` | `lexicon_v3_storage_schema::v3_dialect_rules_migration_backfills_in_order_and_rolls_back` |
| `_sqlx_test_euKYjNYUqKcdOnARBTdGv3_UOi2c30DVxnbIPaypmkDN4Q7397ii` | `lexicon_handler::tmp_probe_variant_component_hard_delete` |
| `_sqlx_test_fO5wkGXDx_Xinn9t64acnd9NV0Jk_A4ZVIicX3UMFCbzzbatjMcT` | `lexicon_handler::tmp_probe_published_zh_translations` |

初次只记录库名，未记录管理表所有列或遗留库内容，不能声称逐字节核对了全部内容。
没有启用逐条SQL审计，Cargo日志也不打印CREATE/DROP，因此没有完整SQL创建/清理日志。
清理结论来自源码、成功结果和当前枚举的联合证据；不能把当前无差异单独当成从未写入证明。

## 修正

发现后全部测试（含lib/bin/doc及子进程）显式使用localhost:55433的
`entry_annotations_gate`。完整lib/bin已在此隔离环境复跑通过；只读核对仍运行的
PID4715、5179、5211实际环境均为此临时实例。
8583、专项HTTP及词库集成测试始终显式指向授权隔离环境。未修改前端独占center组。
未对原库进行补偿写入或删除记录。

核对时间：2026-09-06T11:35:24.618658+00:00

## 日志索引与完整性

- `/tmp/entry_annotations_unit_gate.log` — SHA256 `c1120449aa0a86e1ac26c90124762a1f8515a002d5419e41930f13b0450d74bc`
- `/tmp/entry_annotations_unit_gate_final.log` — SHA256 `16c5d5f64c12b65b55f5d3c27c707e1ad6b7fcf7540c17359c26282d073deb4c`
- `/tmp/entry_annotations_unit_gate_isolated.log` — SHA256 `45e727452124b5868630df3a1d7e18368a2053e43c40ae94191d8d156cac9273`
- `/tmp/entry_annotations_lexicon_gate.log` — SHA256 `6192cdf75cd082e96ac846b98b2c46f8b8868f3b1d49bcf8f41066fb34ee2933`
- `/tmp/entry_annotations_lexicon_gate_final.log` — SHA256 `a10d0e2493e56c0ca9c05bc1c2dcf5841eb079d2ddb039f30a210441988a8386`
- `/tmp/entry_annotations_remaining_tests.log` — SHA256 `95a0e64d53e26953935b426e99d0610ce14624b42d8e5ddca3df7ef8a90b3efd`
- `/tmp/entry_annotations_race_test.log` — SHA256 `f9368310200b4fe1d8a054bea34e3fd77296e0537fb0f944f7627584e9852b43`
- `/tmp/entry_annotations_remaining_gate.log` — SHA256 `b21dab6d82ea49a76372f4b1322e110389481dd0485db973a63a12667dbb8e5c`
- `/tmp/entry_annotations_clippy_final.log` — SHA256 `58f7149ec4935a01a0cf5f603ceabecbdb6d21fc393909866fe21e970042790d`
- `/tmp/entry_annotations_doc.log` — SHA256 `5bd897f808d72ea8c0b9be532b82327158c52f9ea2efad7c22e6f3296c0559f2`
