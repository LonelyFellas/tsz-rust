# PostgreSQL 备份与恢复验证

本 runbook 用于在受控环境中创建写前恢复点，并在不连接业务网络的一次性 PostgreSQL 容器中验证恢复。它不安装系统包、不拉取镜像、不修改服务配置，也不把 `DATABASE_URL` 或密码写入命令参数、日志或制品。

## 门禁

开始前必须确认：

- 当前操作目标不是生产环境；
- 应用使用的环境文件路径来自 `systemctl show tsz-rust -p EnvironmentFiles --value`，不猜测主机或凭据；
- 数据库服务端版本已通过只读 SQL 确认；
- 宿主已缓存并审核 PostgreSQL 18 镜像，且使用不可变 digest；缺失时停止并申请镜像拉取授权；
- 备份目录有足够空间，恢复目标名称、数据库名和清理动作均已显式记录。

tshb-test 当前审核基线：

```text
image=postgres@sha256:d3e1620b530c944afa6e887d22eb899824da68e19c52024bf98f5220c88a65b2
pg_dump_version=18.6
```

镜像升级必须重新记录 digest、`pg_dump --version` 和恢复验证结果，不能只复用可变的 `postgres:18-alpine` tag。

## tshb-test 发布与独立备份

本环境尚未正式上线。普通兼容程序发布不等待本次整库导出、下载或完整恢复；发布前记录独立备份状态，失败或延迟明确告警。判断兼容性必须覆盖持久化格式和后台副作用，不能仅看有没有 migration。数据风险发布仍需匹配本次风险的写前备份与专项验证，缺失即停止。

独立任务入口为 `ops/postgresql_backup_job.py`；`ops/systemd/tsz-backup.service` 和 `.timer` 是待安装模板，本次实现不自动安装或启用。默认每日服务器本地时间 03:17 后随机延迟最多 10 分钟运行，45 分钟超时；失败通过非零退出与 systemd failed 状态报告，安装时必须接入既有通知通道。`ops/postgresql_restore_job.py` 与 `ops/systemd/tsz-restore-verification.service/.timer` 提供每周日 05:23 后的独立恢复模板：取回最近一次成功异机备份，核对原始摘要与字节数，在不可变镜像、无网络、限 CPU/内存的独占容器恢复，检查表和 SQLx 成功账本，先通过 TCP 对目标数据库执行 `SELECT 1`，避开初始化临时服务；恢复后按持久记录的 owner 与完整 ID 清理容器、匿名数据卷及本地下载副本，全部完成后才写成功的 `restore-latest.json`。Docker 创建响应丢失时仍按 owner 查询，取消信号不作为就绪重试；清理失败保持 `cleanup_complete=false`。工具主版本或镜像变化后追加验证。该自动检查证明归档可恢复与基本账本可读，不替代下述关键业务 row count/checksum 专项审计；日常归档可读也不等于完整恢复验证通过。

受保护的 `/etc/tsz-rust/backup.json` 配置示例：

```json
{
  "environment": "tshb-test",
  "directory": "/opt/tsz-rust/backups/scheduled",
  "postgresql_major": 18,
  "archive_command": ["/usr/bin/python3", "-I", "/opt/tsz-rust/ops/backup_archive_ssh.py", "/etc/tsz-rust/backup-storage.json"],
  "fetch_command": ["/usr/bin/python3", "-I", "/opt/tsz-rust/ops/backup_archive_ssh.py", "fetch", "/etc/tsz-rust/backup-storage.json"],
  "restore_image": "postgres@sha256:d3e1620b530c944afa6e887d22eb899824da68e19c52024bf98f5220c88a65b2"
}
```

`postgresql_major` 按只读核实的目标填写；任务要求预装的 `psql`、`pg_dump`、`pg_restore` 与之匹配，不自动安装或升级。数据库 URI 只从正式进程读取并通过子进程环境传入，不进入 argv、状态或 journal。任务串行互斥，状态文件替换后同步父目录；外部传输在超时或取消时终止整个本地进程组，再释放锁。dump 写入 `.partial`，工具可读后核对 SHA256，再调用受信任的异机存储传输程序。该程序接收固定归档绝对路径和摘要，完成上传及远端读取校验后返回 JSON：`verified=true`、`sha256`、`bytes`、`reference`；未配置或失败不会生成成功状态。传输程序只接收路径与摘要，不能获得数据库连接环境；安装前须核对其权限、目的地及返回校验，不能以仅上传成功冒充异机可读。

内置 `ops/backup_archive_ssh.py` 支持受审阅 SSH 目的地，存储配置为 `{"host":"已配置的SSH别名","directory":"/absolute/backup/root"}`；连接必须开启主机密钥校验。实际值由运维配置，别名不能指回源数据库机器；首次启用前核实是独立故障域。它以 rsync 传固定文件、在远端复算摘要与字节数，再原子完成命名；测试不连接真实 SSH。不能在配置未知时自动安装任务。

也可使用 `ops/backup_archive_oss.py` 将备份保存到独立私有 OSS Bucket。使用独立受限 RAM 用户，限定到专用 Bucket 和 `database-backups/`，不得复用业务上传凭据；在受保护的 root 所有、权限 `0600` 的 `/etc/tsz-rust/backup-oss.json` 配置 `endpoint`、`region`、`bucket`、`access_key_id` 和 `access_key_secret`，并安排凭据轮换。只允许阿里云 HTTPS endpoint。为避免 OSS 在开启或暂停版本控制时忽略禁止覆盖的请求，专用 Bucket 必须保持版本控制未启用，并禁止公开访问；还须配置服务端禁止覆盖规则，覆盖 `database-backups/` 全前缀及所有主体。适配器每次运行时读取并核实该规则、私有 ACL 与公共访问阻止设置；访问策略和保留期限须单独核实。写入启用 AES256 服务端加密和对象私有 ACL，上传后重新读取全部字节核对 SHA256 与长度；取回时先核对长度，并在流式读取过程中限制文件大小，再核对摘要，失败删除本次独占的临时文件。

在隔离的受控 Python 环境安装 `oss2==2.19.1` 后，把 `backup.json` 的 `archive_command` 设为 `["/opt/tsz-rust/backup-venv/bin/python", "-I", "/opt/tsz-rust/ops/backup_archive_oss.py", "/etc/tsz-rust/backup-oss.json"]`，`fetch_command` 设为同一列表并在脚本路径后加入 `"fetch"`，同时设 `fetch_expected_bytes: true` 以传递回执中的预期大小。当前使用单次上传，备份超过 5 GiB 时拒绝上传并报警；容量增长前须另行支持分片上传。恢复下载前和创建容器前检查备份目录及 Docker 卷所在文件系统，预留压缩包大小加 5 GiB，以及压缩包 20 倍加 2 GiB 的恢复预算；同一文件系统时两项相加。该预算是入场检查，并非卷配额：数据压缩比异常或其他进程同时占用空间时仍可能不足，须根据实际恢复数据量核实并持续监控。在启用调度前验证私有权限、凭据最小授权、完整上传和取回、隔离恢复与失败通知；仅以模拟 OSS 通过不能称为真实归档成功。对象保留和清理由存储端配置，脚本不删除正式备份。

若使用云监控通知，使用独立 RAM 用户与独立的 root 所有、权限 `0600` 的 `/etc/tsz-rust/backup-alert.json`，填写 `access_key_id`、`access_key_secret` 和 `monitor_group_id`；该用户只授予 `cms:PutCustomEvent` 和 `cms:PutCustomMetric`。安装 `tsz-backup-alert@.service` 与两个 `*.service.d/10-cloud-alert.conf`，并创建匹配事件名 `tsz_backup_failure` 的邮件报警规则，备份或恢复验证失败时上报不含数据库内容的固定事件。安装 `tsz-backup-heartbeat.timer`，每五分钟上报 `tsz_backup_health`：备份新鲜且成功、备份目录所在文件系统剩余至少 5 GiB 时为 1，缺失、失败、陈旧或空间不足时为 0。须配置独立云端指标报警，包含持续为 0 和无数据两种条件；由于备份过程中状态可能暂时为 0，阈值和持续时间须覆盖 45 分钟任务超时。启用前实际测试事件、指标异常和无数据到邮箱的完整投递；服务器、凭据或网络失效时依靠云端无数据报警，未经验证不得声称失联监控有效。

经授权后，可在 `backup.json` 设置 `local_retention_days`（至少 14），安装 `tsz-backup-prune.service/.timer` 清理本机定时归档。清理必须持有备份和恢复锁，锁忙则跳过并等待下次运行；最新备份须新鲜、深圳副本校验通过。仅删除匹配本任务格式、受保护的目录下有成功回执且到期的 `backup.dump`，并在每个删除前从深圳读回对应归档，核对完整 SHA256、字节数和加密状态；验证失败则保留该归档、继续处理其他归档并报警。读取每份深圳归档会产生 OSS 公网流量和请求费用；大量积压可能超过单次五分钟时限，需人工处理。始终保留最新归档、回执、失败归档、发布备份与其他文件。失败归档仍需人工核对，不能仅依赖该规则保证磁盘有空间。

跨地域复制采用异步方式，源桶上传成功不代表目标桶已到达。使用该方案时在 `backup.json` 中配置 `replica`：`storage_config` 指向受保护的 OSS 备份凭据文件，`endpoint`、`region`、`bucket` 指向独立目标桶；该凭据需要对两个专用桶的安全配置和备份对象有相应只读权限。心跳检查最近归档在两个桶上的长度、OSS CRC64 和目标端 AES256 加密状态；不存在、权限失效、校验不符或请求失败都会上报 0。刚完成上传时复制仍可能进行中，云端报警持续时间应覆盖正常复制延迟；CRC64 适合持续检查，不替代首次及定期的完整 SHA256 下载和恢复演练。

建议异机至少保留近 7 天，存储生命周期由受控目的地配置管理；此工具不自动删除历史备份。`latest.json` 指向最后一次成功的异机归档，`last-attempt.json` 独立记录最近任务，失败或中断不会被旧成功掩盖；`status` 以快照开始时间检查 26 小时新鲜度，具体失败回执保留在任务目录。完整备份不自动包含 OSS 文件，需要独立检查对象版本保留。发布侧只读查看该状态，不触发本任务；真正的数据风险发布不能用普通状态替代写前保护。

## 创建恢复点

下列命令从运行中服务的 `/proc/<MainPID>/environ` 读取 systemd 已解析的单个 `DATABASE_URL`，不使用 shell `source` 重新解释环境文件。Python 将 URI 拆成 libpq service 参数和密码；两者均通过 FIFO 传入容器，密码只进入容器内 `pg_dump` 的进程环境，argv 和 Docker 配置不含 DSN 或密码。dump 和校验文件权限为 `0600`。不要启用 shell tracing。

```bash
set -euo pipefail
image='postgres@sha256:d3e1620b530c944afa6e887d22eb899824da68e19c52024bf98f5220c88a65b2'
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup_dir='/opt/tsz-rust/backups'
base="j2-preflight-${stamp}"
main_pid="$(systemctl show tsz-rust -p MainPID --value)"
secret_dir="$(mktemp -d /tmp/tsz-backup.XXXXXX)"
service_fifo="$secret_dir/pg_service.conf"
password_fifo="$secret_dir/password"

test "$main_pid" -gt 0
test -r "/proc/$main_pid/environ"
install -d -m 0700 "$backup_dir"
mkfifo -m 0600 "$service_fifo" "$password_fifo"
trap 'rm -rf "$secret_dir"' EXIT

network_mode="$(python3 - "$main_pid" <<'PY'
import ipaddress
import sys
from urllib.parse import urlsplit

pid = sys.argv[1]
items = open(f"/proc/{pid}/environ", "rb").read().split(b"\0")
raw = next((item.split(b"=", 1)[1] for item in items if item.startswith(b"DATABASE_URL=")), None)
if not raw:
    raise SystemExit("DATABASE_URL is absent from service process")
url = urlsplit(raw.decode("utf-8"))
if url.scheme not in {"postgres", "postgresql"} or not url.hostname:
    raise SystemExit("unsupported DATABASE_URL")
try:
    loopback = ipaddress.ip_address(url.hostname).is_loopback
except ValueError:
    loopback = url.hostname == "localhost"
print("host" if loopback else "bridge")
PY
)"

write_libpq_fifos() {
  python3 - "$main_pid" "$service_fifo" "$password_fifo" <<'PY'
import sys
from urllib.parse import parse_qsl, unquote, urlsplit

pid, service_fifo, password_fifo = sys.argv[1:]
items = open(f"/proc/{pid}/environ", "rb").read().split(b"\0")
raw = next((item.split(b"=", 1)[1] for item in items if item.startswith(b"DATABASE_URL=")), None)
if not raw:
    raise SystemExit("DATABASE_URL is absent from service process")
url = urlsplit(raw.decode("utf-8"))
if url.scheme not in {"postgres", "postgresql"} or not url.hostname or not url.path.lstrip("/"):
    raise SystemExit("unsupported DATABASE_URL")

host = url.hostname
port = str(url.port or 5432)
user = unquote(url.username or "")
password = unquote(url.password or "")
database = unquote(url.path.lstrip("/"))
params = [("host", host), ("port", port), ("user", user), ("dbname", database)]
allowed = {"sslmode", "connect_timeout", "target_session_attrs", "channel_binding", "gssencmode"}
for key, value in parse_qsl(url.query, keep_blank_values=True):
    if key not in allowed:
        raise SystemExit(f"unsupported connection parameter: {key}")
    params.append((key, value))
if any(not value or value != value.strip() or "\n" in value or "\r" in value for _, value in params):
    raise SystemExit("unsupported whitespace or empty connection parameter")

if "\n" in password or "\r" in password:
    raise SystemExit("unsupported newline in password")
with open(password_fifo, "w", encoding="utf-8", buffering=1) as password_file:
    password_file.write(password)
with open(service_fifo, "w", encoding="utf-8", buffering=1) as service:
    service.write("[backup]\n" + "".join(f"{key}={value}\n" for key, value in params))
PY
}

write_libpq_fifos &
docker_network=()
test "$network_mode" != host || docker_network=(--network host)
docker run --rm \
  "${docker_network[@]}" \
  -v "$service_fifo:/run/pg_service.conf:ro" \
  -v "$password_fifo:/run/password:ro" \
  -v "$backup_dir:/backup" \
  "$image" sh -ceu \
  'PGPASSWORD="$(cat /run/password)"; export PGPASSWORD PGSERVICEFILE=/run/pg_service.conf PGSERVICE=backup; exec pg_dump --format=custom --compress=9 --no-owner --no-privileges --file="/backup/'"$base"'.dump.partial"'
wait

chmod 0600 "$backup_dir/$base.dump.partial"
mv "$backup_dir/$base.dump.partial" "$backup_dir/$base.dump"
(
  cd "$backup_dir"
  sha256sum "$base.dump" > "$base.dump.sha256"
)
chmod 0600 "$backup_dir/$base.dump.sha256"
```

失败或中断时，不得把零字节/不完整文件当成恢复点。先用 `pg_restore --list` 和 `sha256sum -c` 验证，再决定是否清理明确命名的失败制品。

## 隔离恢复

恢复容器不映射端口并使用 `--network none`；数据库文件位于 tmpfs，删除容器即清除。PostgreSQL 18 官方镜像的数据目录挂载点是 `/var/lib/postgresql`，不要沿用旧版的 `/var/lib/postgresql/data`。

```bash
set -euo pipefail
image='postgres@sha256:d3e1620b530c944afa6e887d22eb899824da68e19c52024bf98f5220c88a65b2'
backup_dir='/opt/tsz-rust/backups'
dump_file='j2-preflight-<UTC timestamp>.dump'
container="tsz-j2-restore-${dump_file%.dump}"

existing="$(docker container ls -a --filter "name=^/${container}$" --format '{{.Names}}')"
if test -n "$existing"; then
  printf 'refusing to replace existing container: %s\n' "$container" >&2
  exit 1
fi
cleanup() {
  status=$?
  if ! existing="$(docker container ls -a --filter "name=^/${container}$" --format '{{.Names}}')"; then
    printf 'cannot verify restore container state: %s\n' "$container" >&2
    status=1
  elif test -n "$existing" && ! docker rm -f "$container" >/dev/null; then
    status=1
  fi
  if ! existing="$(docker container ls -a --filter "name=^/${container}$" --format '{{.Names}}')"; then
    printf 'cannot verify restore container cleanup: %s\n' "$container" >&2
    status=1
  elif test -n "$existing"; then
    printf 'restore container cleanup failed: %s\n' "$container" >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
(
  cd "$backup_dir"
  sha256sum -c "$dump_file.sha256"
)
docker run -d --name "$container" --network none \
  -e POSTGRES_HOST_AUTH_METHOD=trust \
  -e POSTGRES_DB=restore_verify \
  --tmpfs /var/lib/postgresql:rw,size=1g \
  -v "$backup_dir:/backup:ro" \
  "$image"

for attempt in $(seq 1 30); do
  docker exec "$container" pg_isready -U postgres -d restore_verify >/dev/null 2>&1 && break
  test "$attempt" -lt 30
  sleep 1
done

docker exec "$container" pg_restore \
  --exit-on-error --no-owner --no-privileges \
  -U postgres -d restore_verify "/backup/$dump_file"
```

## 验证与审计

至少记录以下无敏感信息的证据：

- UTC 创建/验证时间、Git SHA、数据库服务端版本；
- 镜像 digest、`pg_dump`/`pg_restore` 版本；
- dump 路径、字节数、SHA-256；
- 源库与恢复库的关键 schema 集合、精确 row count；
- 对关键表执行确定性内容 checksum。包含 `timestamptz` 时，两个会话必须先 `SET timezone='UTC'`，否则相同数据可能因会话时区产生不同文本摘要；
- `pg_restore --exit-on-error` 退出状态；
- 恢复容器名称、数据库名、`network:none`、tmpfs 挂载和最终清理结果。

只有 hash 校验、恢复命令、schema/row count/checksum 对比和临时资源清理全部通过，本次完整恢复演练才是 **PASS**。任何一项缺失均为 **FAIL** 或 **BLOCKED**，不得声称该备份已完整恢复验证。源库活跃时，比较基线必须来自 dump 的同一一致快照，不能要求快照后的正常业务写入与恢复库相等。普通兼容程序发布与完整演练解耦；需要本次数据保护的高风险操作仍须在证据齐备后执行。
