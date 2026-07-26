---
name: deploy
description: 通过 ssh 把 tsz-rust 当前工作区代码自动化部署到生产服务器 tshb-test（rsync → 服务器编译 → 重启 → 冒烟验证 → 可回退）。用户说「部署」「deploy」「推到服务器」「上线」「更新服务器代码」或提到 tshb-test / 47.121.142.19 的代码同步时使用。
---

# tsz-rust 部署到 tshb-test

把**本地工作区**（不是 git HEAD——rsync 推的是磁盘上的文件）部署到
Aliyun ECS `tshb-test`（47.121.142.19，ssh 别名已配密钥）。服务器上
`/opt/tsz-rust` 不是 git 仓库，唯一同步方式就是本流程。

## 前置检查（跳过会在后面炸）

1. **确认要部署的就是工作区现状**：`git status` 给用户看一眼有哪些未提交改动会被推上去；
   工作区领先于 git 是允许的（历史上干过），但要说清楚。
2. **sqlx 离线缓存**：服务器用 `SQLX_OFFLINE=true` 编译，本地新增/修改过 `query!` 宏
   而没刷缓存 → 服务器编译必失败。检查 `git status --porcelain .sqlx`，有疑问就刷：
   ```bash
   cargo sqlx prepare -- --all-targets   # 需要本地 Docker Postgres(5433) 在跑
   ```
3. **ssh 连通性**：`ssh -o ConnectTimeout=8 tshb-test echo ok`。连不上的两大原因（都遇过）：
   - 本机 Clash/Surge 开着 TUN 模式，把 ssh 劫走非白名单出口 → 让用户关代理或给
     47.121.142.19 加 DIRECT 规则；
   - 家用动态 IP 变了，Aliyun 安全组白名单失效 → 让用户去控制台给新 IP 加 /32。

## 部署步骤

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

## 冒烟验证（部署不验证 = 没部署）

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

## 回退

```bash
ssh tshb-test 'cp /opt/tsz-rust/tsz-rust.bak-<时间戳> /opt/tsz-rust/target/release/tsz-rust && systemctl restart tsz-rust'
```

代码文件不用回退（下次 rsync 会覆盖），二进制换回即服务回退。含迁移的改动回退要慎重——
迁移已作用于 RDS，回退二进制前先确认旧代码兼容新 schema。

## 环境变量

`/opt/tsz-rust/.env`（rsync 永远排除，改它只能 ssh 手动）当前含：
PORT / DATABASE_URL（外部 Aliyun RDS）/ JWT_SECRET / REDIS_URL / **COOKIE_SECURE=false**
（域名备案中、无 TLS 的临时项；备案后上 Caddy TLS 时删除恢复默认 true）。
新增配置项时记得同步：`.env.example`、`docs/deployment.md` §3、以及服务器 .env。
