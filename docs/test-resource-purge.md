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

对象删除失败时数据库记录保留，重复相同 `request_id` 可以安全重试。数据库删除成功后会保留 purge 幂等记录，重复请求只返回原结果。

普通词条创建请求不携带 `X-TSZ-Test-Run`，因此不会进入测试批次，也不能被该接口清理。
