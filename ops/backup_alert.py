#!/usr/bin/env python3
"""Report a backup failure to CloudMonitor without sending database details."""

import json
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from backup_archive_oss import ArchiveError, read_config


UNITS = {
    "tsz-backup.service",
    "tsz-backup-status.service",
    "tsz-restore-verification.service",
}


def report(config, unit, client_factory=None):
    if unit not in UNITS:
        raise ArchiveError("invalid_alert_unit")
    group_id = str(config.get("monitor_group_id", ""))
    if not re.fullmatch(r"[1-9][0-9]*", group_id):
        raise ArchiveError("invalid_monitor_group")
    if client_factory is None:
        from aliyunsdkcore.client import AcsClient
        from aliyunsdkcore.request import CommonRequest
        client_factory = lambda: (
            AcsClient(config["access_key_id"], config["access_key_secret"], "cn-hangzhou"),
            CommonRequest(),
        )
    client, request = client_factory()
    request.set_domain("metrics.cn-hangzhou.aliyuncs.com")
    request.set_version("2019-01-01")
    request.set_action_name("PutCustomEvent")
    request.set_method("POST")
    request.set_protocol_type("https")
    request.add_query_param("EventInfo.1.EventName", "tsz_backup_failure")
    request.add_query_param("EventInfo.1.GroupId", group_id)
    request.add_query_param("EventInfo.1.Content", "tshb-test backup check failed: " + unit)
    response = json.loads(client.do_action_with_exception(request))
    if str(response.get("Code")) != "200":
        raise ArchiveError("alert_report_failed")
    return {"status": "reported", "unit": unit}


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3:
            raise ArchiveError("invalid_arguments")
        print(json.dumps(report(read_config(Path(sys.argv[1])), sys.argv[2])))
    except Exception as error:
        code = str(error) if isinstance(error, ArchiveError) else "alert_report_failed"
        print(json.dumps({"error": code}), file=sys.stderr)
        raise SystemExit(1)
