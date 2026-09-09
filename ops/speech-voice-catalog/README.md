# 音色目录

- catalog-snapshot.json：2026-09-09 从本任务已完成 Azure 同步的隔离目录导出的 73 个 GA 英美 Neural 音色，保留各自 StyleList。
- product.json：8 个常用项的中英文名称和展示顺序。

两份配置编译进后端。启动时仅补缺，不改已有 ID、alias、enabled、能力与限幅。
GET /api/v1/admin/speech/voices 只读全部已启用英美目录，返回 display_name 和 is_common。
非常用音色仍可选择，前端默认折叠；没有选择配置时全部不勾选。

显式设置目标 DATABASE_URL 与 Azure Speech 配置后，可执行能力更新：

```bash
cargo run --locked --bin sync_speech_voices
```

先获取 Azure 目录并核齐常用项，再事务同步所有合格音色；失败不改库。
版本按目录事实 hash 生成，保留人工停用和限幅；不删除历史记录。
新增/更新预置目录时同步维护 snapshot；中文名称属于产品配置。

seed.sql 保留旧版 Aria、Davis、Sonia 初始化数据，新服务不依赖手动执行它。
勾选只保存未来 C 端音色配置，当前没有自动为 C 端生成音频的任务。
