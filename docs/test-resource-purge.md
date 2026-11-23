# 测试资源精确清理

测试资源 purge 只在显式配置 `TEST_RESOURCE_PURGE_ENABLED=true` 且配置了 `TEST_RESOURCES_*` 白名单时开放。请求必须携带 `X-TSZ-Test-Run`，管理员必须属于 `TEST_RESOURCES_ADMIN_IDS`，词条也必须在创建时携带同一批次头并登记到该批次。

```http
POST /api/v1/admin/lexicon-next/test-resources/purge
X-TSZ-Test-Run: codex-accept-20261122-1
Content-Type: application/json

{
  "request_id": "<new-request-uuid>",
  "entry_ids": ["<exact-entry-uuid>"]
}
```

服务端只接受已归档词条，并在删除前锁定词条、发布记录和音频资产，检查拓展词、例句、语法、方言配对及跨批次音频引用。正式存储根目录中的音频会直接拒绝处理，只有 `temporary-tests/<run>/audio/` 下的对象才会进入删除阶段。

清理音频时，测试存储凭据需要能列举对象版本并按版本永久删除。服务端逐个核对对象键，删除全部版本和删除标记，再次列举确认为空后才删除数据库登记。列举、删除或复查失败时保留登记，可用相同请求重试。

同一管理员并发提交相同的清理请求时，共用第一次成功的回执。清理会移除目标全部属于本次词条的归档和恢复回执，保留含其他词条的回执以及 purge 自身的幂等记录。

普通词条创建请求不携带 `X-TSZ-Test-Run`，因此不会进入测试批次，也不能被该接口清理。
