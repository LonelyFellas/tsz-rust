---
name: deploy
description: 仅在 GitHub origin/main 当前提交的 CI 成功后，通过 ssh 把该提交自动化部署到生产服务器 tshb-test（CI 门禁 → rsync → 服务器编译 → 重启 → 冒烟验证 → 可回退）。用户说「部署」「deploy」「推到服务器」「上线」「更新服务器代码」或提到 tshb-test / 47.121.142.19 的代码同步时使用。
---

# tsz-rust 部署到 tshb-test

把 GitHub **当前 `origin/main` 上 CI 成功的精确提交**部署到 Aliyun ECS
`tshb-test`（47.121.142.19，ssh 别名已配密钥）。服务器上
`/opt/tsz-rust` 不是 git 仓库，唯一同步方式就是本流程。

## 红线

- 只部署干净工作区中的本地 `main`，且 `HEAD` 必须等于 GitHub `origin/main`。
- 只把 CI `conclusion == success` 视为通过；`skipped`、`neutral` 也不允许部署。
- CI 为 `queued`、`in_progress`、`pending`、`requested` 或 `waiting` 时等待，不得提前部署。
- CI 失败、取消、超时、无运行记录、GitHub API/认证异常或状态未知时，一律不部署。
- 等待期间 `origin/main` 变化时，重新同步并从新 SHA 开始检查，绝不沿用旧提交的绿色结果。
- CI 门禁通过前，不得备份、rsync、编译、重启或执行任何会改变服务器状态的命令。

## 1. 准备精确的 main 源码

1. 运行 `git status --porcelain`。存在任何已跟踪或未跟踪改动就停止；不得部署未提交工作区。
2. 安全切换并快进本地 `main`：
   ```bash
   git switch main
   git fetch origin main
   git merge --ff-only origin/main
   ```
   切换或快进失败就停止；不得 reset、丢弃或覆盖本地改动。
3. 记录部署 SHA 和主题：
   ```bash
   deploy_sha=$(git rev-parse HEAD)
   git show -s --format='%H %s' "$deploy_sha"
   test "$(git branch --show-current)" = main
   test "$deploy_sha" = "$(git rev-parse origin/main)"
   test -z "$(git status --porcelain)"
   ```

## 2. 强制等待 GitHub main CI 成功

调用技能内的确定性门禁脚本；默认最多等待 30 分钟、每 15 秒查询一次：

```bash
.agents/skills/deploy/scripts/require-green-main.sh "$deploy_sha"
```

运行期间持续向用户报告等待状态，不要让用户超过 60 秒收不到进展。仅退出码 `0` 允许继续：

- `0`：精确 SHA 的最新 `CI` 工作流已完成，且结论为 `success`；
- `1`：CI 已完成但失败或为非成功结论，停止部署；
- `2`：工作区/分支不安全、无 CI、查询/认证/状态异常或等待超时，停止部署；
- `3`：等待期间 `origin/main` 变化，回到第 1 步同步新提交并重新检查。

门禁成功后、任何服务器写操作前，再做一次防竞态复核：

```bash
git fetch origin main
test "$(git branch --show-current)" = main
test "$(git rev-parse HEAD)" = "$deploy_sha"
test "$(git rev-parse origin/main)" = "$deploy_sha"
test -z "$(git status --porcelain)"
```

任一检查失败就停止并重新走 CI 门禁。

## 3. 其他前置检查

1. **sqlx 离线缓存**：服务器用 `SQLX_OFFLINE=true` 编译；CI 已用提交的 `.sqlx/` 离线缓存
   编译并检查。此时不得在本地刷新或修改缓存，否则工作区不再等于已验证提交。
2. **ssh 连通性**：`ssh -o ConnectTimeout=8 tshb-test echo ok`。连不上的两大原因（都遇过）：
   - 本机 Clash/Surge 开着 TUN 模式，把 ssh 劫走非白名单出口 → 让用户关代理或给
     47.121.142.19 加 DIRECT 规则；
   - 家用动态 IP 变了，Aliyun 安全组白名单失效 → 让用户去控制台给新 IP 加 /32。

## 4. 部署步骤

```bash
# 1) 备份当前二进制（回退保险）
ssh tshb-test 'cp /opt/tsz-rust/target/release/tsz-rust /opt/tsz-rust/tsz-rust.bak-$(date +%m%d-%H%M)'

# 2) 推代码。排除项一个都不能少：
#    .env 是生产密钥(600 权限,含 JWT_SECRET/COOKIE_SECURE)——rsync 覆盖或 --delete 掉它=事故;
#    target 巨大且服务器要自己编译; 绝不使用 --delete。
rsync -az --exclude .git --exclude target --exclude .env --exclude .vscode \
  /Users/darwish/Dev/tsz-core/tsz-rust/ tshb-test:/opt/tsz-rust/

# 3) 服务器编译(后台跑,增量 1-2 分钟,全量约 7 分钟)。
#    机器只有 3.4G 内存+2G swap,JOBS=2 防 OOM,别调高。
ssh tshb-test 'cd /opt/tsz-rust && bash -o pipefail -c "CARGO_BUILD_JOBS=2 SQLX_OFFLINE=true ~/.cargo/bin/cargo build --release 2>&1 | tail -3"'

# 4) 重启 + 基础确认
ssh tshb-test 'systemctl restart tsz-rust && sleep 2 && systemctl is-active tsz-rust && curl -s http://127.0.0.1:8383/healthz'
```

编译步骤用后台执行（Bash run_in_background），完成后再继续,不要阻塞等待。

## 5. 冒烟验证（部署不验证 = 没部署）

服务器本机跑,B=`http://127.0.0.1:8383/api/v1`：

1. `healthz` → `{"status":"ok"}`；
2. `POST $B/auth/refresh`（无 cookie）→ **401 `{"error":"invalid refresh token"}`**
   （若返回 422 说明旧二进制没换掉）；
3. 全链路（使用常驻冒烟账号；先执行
   `set -a; source ~/.config/tsz-rust/deploy-smoke.env; set +a`，再从环境变量
   `TSZ_SMOKE_IDENTIFIER` / `TSZ_SMOKE_PASSWORD` 读取凭据；禁止写入仓库、命令参数或日志）：
   login 请求体字段是 **`identifier`**（统一承接手机号/邮箱，不叫 `phone`——用错必 422）：
   ```json
   {"identifier": "$TSZ_SMOKE_IDENTIFIER", "password": "$TSZ_SMOKE_PASSWORD"}
   ```
   login 200 且 Set-Cookie 含 `HttpOnly; SameSite=Lax; Path=/api/v1/auth`（**不含
   Secure**——服务器 .env 设了 COOKIE_SECURE=false，见下）→ 拿 cookie 刷新 200 且
   轮换出新值 → 带新 cookie logout 204；
4. 若本次改动含**新迁移**：`journalctl -u tsz-rust -n 50 | grep -i migrat` 确认
   「database migrations applied」（启动自动迁移,连的是外部 RDS）。

把冒烟结果整理成表格报告给用户。

## 6. 回退

```bash
ssh tshb-test 'cp /opt/tsz-rust/tsz-rust.bak-<时间戳> /opt/tsz-rust/target/release/tsz-rust && systemctl restart tsz-rust'
```

代码文件不用回退（下次 rsync 会覆盖），二进制换回即服务回退。含迁移的改动回退要慎重——
迁移已作用于 RDS，回退二进制前先确认旧代码兼容新 schema。

## 7. 环境变量

`/opt/tsz-rust/.env`（rsync 永远排除，改它只能 ssh 手动）当前含：
PORT / DATABASE_URL（外部 Aliyun RDS）/ JWT_SECRET / REDIS_URL / **COOKIE_SECURE=false**
（域名备案中、无 TLS 的临时项；备案后上 Caddy TLS 时删除恢复默认 true）。
新增配置项时记得同步：`.env.example`、`docs/deployment.md` §3、以及服务器 .env。
