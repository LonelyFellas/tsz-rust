# 依赖安全检查

CI 在 PR、main push 和手动运行时使用固定版本 `cargo-audit 0.22.2` 检查完整 `Cargo.lock`，包括可选功能和构建依赖。新漏洞、撤回版本及依赖维护警告会使总门禁失败。每周一 03:23 UTC 另跑一次审计，以发现锁文件未变时新增的公告；定时运行不重复构建和数据库测试。

本地复验：

```bash
cargo install cargo-audit --version 0.22.2 --locked
cargo audit --deny warnings
```

工具默认更新 RustSec 数据库；不要用过期数据库或 `--no-yanked` 代替正常门禁。例外统一放在 [`.cargo/audit.toml`](../.cargo/audit.toml)，不能通过降低严重级别或忽略全部警告放行。

## 当前例外

`RUSTSEC-2023-0071`（`rsa` Marvin 时序侧信道），核实日期 2026-10-22，下次复查不晚于 2027-01-22：

- `rsa` 由 `jsonwebtoken` 的 `rust_crypto` 聚合功能引入，上游公告尚无修复版本。仅升级 `jsonwebtoken` 仍会引入该依赖。
- Web 和 admin 的 `TokenManager` 均只用共享密钥签发、校验 HS256；没有 RSA 私钥签名或解密路径。`auth::token::tests::both_realms_only_accept_hs256` 固定算法白名单，并验证拒绝其他 HMAC 算法。
- 修改 JWT 算法、密钥类型或加密 provider 前必须重新评估，不得沿用本例外。上游提供修复或能移除该依赖时，升级并删除例外；提前出现新的使用路径时也必须重新处理。
- 参考：[RustSec 公告](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)、[上游跟踪](https://github.com/RustCrypto/RSA/issues/626)。

2026-10-22 已将 `utoipa-swagger-ui` 升至 10.0.2，其构建依赖使用 `zip 9`，无需忽略 `RUSTSEC-2026-0367`、`RUSTSEC-2026-0368` 或 `RUSTSEC-2026-0369`。Swagger 仍只在显式启用 `swagger` 功能时提供；升级时须验证 OpenAPI 生成、前端契约及页面资源。
