#!/usr/bin/env bash
set -euo pipefail

configuration=$(ssh -G tshb-test)
python3 - "$configuration" <<'PY'
from ipaddress import IPv4Address, IPv4Network
import sys

settings = dict(line.split(" ", maxsplit=1) for line in sys.argv[1].splitlines() if " " in line)
try:
    target = IPv4Address(settings.get("hostname", ""))
except ValueError:
    raise SystemExit("tshb-test needs a literal RFC 1918 IPv4 address in SSH config") from None

private_networks = ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")
if not any(target in IPv4Network(block) for block in private_networks):
    raise SystemExit("tshb-test needs a literal RFC 1918 IPv4 address in SSH config")

if any(settings.get(key, "none") != "none" for key in ("proxycommand", "proxyjump", "controlpath")):
    raise SystemExit("tshb-test SSH must connect directly with no proxy or shared socket")
PY
ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=10 tshb-test true
