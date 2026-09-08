#!/usr/bin/env python3
"""本地 dev 的一次性测试管理员：一键建、能登录、用完删。

用来验证那些「必须换个身份才看得见」的行为——比如草稿的写权限收口，超管和创建者
本人都看不出效果，非得用另一个普通管理员登录才能确认按钮真的置灰了。

**只对本地 dev 库工作。** 库地址不是 localhost/127.0.0.1 一律拒绝执行：这个脚本会
写 admins 表，绝不能对着测试服或生产跑。

口令与验证码：
- 密码由 `cargo run --bin seed` 走后端自己的策略校验落库，`must_change_password=false`，
  所以建完就能直接登录，不会卡在强制改密页。
- OTP 在 mock 通道固定 `000000`（src/otp/sender.rs），dev 环境登录直接用它。

用法：
    python3 ops/dev_test_admins.py create     # 建号并打印凭据
    python3 ops/dev_test_admins.py login      # 验证两个号都能真登录，打印 access token
    python3 ops/dev_test_admins.py cleanup    # 删号
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from urllib.parse import urlparse

REPO = Path(__file__).resolve().parent.parent

# 固定手机号前缀，便于识别与清理；正常业务号不会落在这一段。
PHONE_SUPER = "19900000001"
PHONE_REGULAR = "19900000002"
PASSWORD = "DevTest!2026"
OTP_CODE = "000000"

MANAGED_PHONES = (PHONE_SUPER, PHONE_REGULAR)


def env_from_dotenv() -> dict[str, str]:
    text = (REPO / ".env").read_text(encoding="utf-8")
    return dict(re.findall(r"^([A-Z_]+)=([^\s#]+)", text, re.M))


def require_local_database(env: dict[str, str]) -> str:
    """写 admins 表之前先证明目标是本地库，不是测试服/生产。"""
    url = env.get("DATABASE_URL", "")
    host = urlparse(url).hostname
    if host not in ("localhost", "127.0.0.1", "::1"):
        sys.exit(
            f"拒绝执行：DATABASE_URL 指向 {host!r}，不是本地库。\n"
            "这个脚本只用于本地 dev，不对测试服或生产账号做任何事。"
        )
    return url


def psql(url: str, sql: str) -> str:
    """通过 docker 里的 psql 执行——本机不一定装了匹配版本的客户端。"""
    result = subprocess.run(
        ["docker", "exec", "-i", "tsz-rust-db-1", "psql", "-U", "postgres",
         "-d", urlparse(url).path.lstrip("/"), "-t", "-A", "-c", sql],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        sys.exit(f"psql 失败：{result.stderr.strip()}")
    return result.stdout.strip()


def api(base: str, path: str, body: dict, cookie: str | None = None):
    req = urllib.request.Request(
        base + path, data=json.dumps(body).encode(), method="POST"
    )
    req.add_header("Content-Type", "application/json")
    if cookie:
        req.add_header("Cookie", cookie)
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, json.loads(resp.read() or b"{}"), resp.headers
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}"), e.headers
    except urllib.error.URLError as e:
        sys.exit(f"连不上后端：{e}. 先把 tsz-rust 跑起来（端口 8383）。")


def cmd_create(env: dict[str, str], url: str) -> None:
    # 超管走后端自己的 seed：密码过策略校验，must_change_password 置 false。
    seed = subprocess.run(
        ["cargo", "run", "--quiet", "--bin", "seed"],
        cwd=REPO, capture_output=True, text=True,
        env={**__import__("os").environ, **env,
             "SEED_ADMIN_PHONE": PHONE_SUPER,
             "SEED_ADMIN_PASSWORD": PASSWORD,
             "SEED_ADMIN_DISPLAY_NAME": "Dev Test SuperAdmin"},
    )
    if seed.returncode != 0:
        sys.exit(f"seed 失败：{seed.stderr.strip()}")

    # 普通管理员复用同一个 password_hash——后端只比对哈希，不关心它从哪来，
    # 这样第二个号不必再走一次策略校验，也不用在这里自己实现 bcrypt。
    psql(url, f"""
        INSERT INTO admins (id, phone, display_name, password_hash, role,
                            must_change_password, status)
        SELECT '{uuid.uuid4()}', '{PHONE_REGULAR}', 'Dev Test Admin',
               password_hash, 'admin', FALSE, 'active'
        FROM admins WHERE phone = '{PHONE_SUPER}'
        ON CONFLICT (phone) DO UPDATE
          SET role = 'admin', status = 'active', must_change_password = FALSE,
              locked_until = NULL, failed_login_count = 0;
    """)

    rows = psql(url, f"""
        SELECT phone, role, display_name FROM admins
        WHERE phone IN {MANAGED_PHONES} ORDER BY role DESC;
    """)
    print("已就绪（密码相同，验证码固定 000000）：\n")
    for row in rows.splitlines():
        phone, role, name = row.split("|")
        print(f"  {role:12} {phone}  {PASSWORD}   {name}")
    print("\n  admin 登录页：http://localhost:3001/login")


def cmd_login(env: dict[str, str], url: str) -> None:
    base = "http://localhost:8383/api/v1"
    for phone in MANAGED_PHONES:
        # admin 有自己的发码端点：公开的 /otp/send 没有 admin_login 这个 purpose。
        api(base, "/admin/auth/login-code", {"phone": phone})
        code, body, headers = api(base, "/admin/auth/login",
                                  {"phone": phone, "password": PASSWORD, "code": OTP_CODE})
        if code != 200:
            print(f"  ❌ {phone} 登录失败 → {code} {body.get('code', '')} {body.get('detail', '')}")
            continue
        token = body.get("access_token", "")
        role = psql(url, f"SELECT role FROM admins WHERE phone = '{phone}';")
        print(f"  ✅ {phone} ({role}) 登录成功  token: {token[:32]}…")


def cmd_cleanup(env: dict[str, str], url: str) -> None:
    # 词条的 created_by_admin_id 指向 admins：测试号建过词条就删不掉。
    # 这里如实报出来，由人决定是先清词条还是留着账号，不擅自删业务数据。
    blocked = psql(url, f"""
        SELECT a.phone, count(e.id)
        FROM admins a LEFT JOIN lexicon.entries e ON e.created_by_admin_id = a.id
        WHERE a.phone IN {MANAGED_PHONES}
        GROUP BY a.phone HAVING count(e.id) > 0;
    """)
    if blocked:
        print("以下测试号建过词条，先处理这些词条再删账号：")
        for row in blocked.splitlines():
            phone, n = row.split("|")
            print(f"  {phone} 建了 {n} 条词条")
        print("（不自动删词条：那是业务数据，得你来决定。）")
        return
    psql(url, f"DELETE FROM admins WHERE phone IN {MANAGED_PHONES};")
    left = psql(url, f"SELECT count(*) FROM admins WHERE phone IN {MANAGED_PHONES};")
    print(f"已删除测试管理员，剩余 {left} 个。")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=["create", "login", "cleanup"])
    args = parser.parse_args()

    env = env_from_dotenv()
    url = require_local_database(env)
    {"create": cmd_create, "login": cmd_login, "cleanup": cmd_cleanup}[args.action](env, url)


if __name__ == "__main__":
    main()
