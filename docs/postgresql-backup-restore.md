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

只有 hash 校验、恢复命令、schema/row count/checksum 对比和临时资源清理全部通过，恢复点门禁才是 **PASS**。任何一项缺失均为 **FAIL** 或 **BLOCKED**，不得继续后续业务写入。
