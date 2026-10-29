#!/usr/bin/env python3
"""Report a backup failure to CloudMonitor without sending database details."""

import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys


class AlertError(Exception):
    pass


def read_config(path):
    path = Path(path)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise AlertError("insecure_alert_config")
    config = json.loads(path.read_text())
    if not config.get("access_key_id") or not config.get("access_key_secret"):
        raise AlertError("invalid_alert_config")
    return config


UNITS = {
    "tsz-backup.service",
    "tsz-restore-verification.service",
    "tsz-backup-prune.service",
}


def report(config, unit, client_factory=None):
    if unit not in UNITS:
        raise AlertError("invalid_alert_unit")
    group_id = str(config.get("monitor_group_id", ""))
    if not re.fullmatch(r"[1-9][0-9]*", group_id):
        raise AlertError("invalid_monitor_group")
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
    request.set_accept_format("json")
    request.add_query_param("EventInfo.1.EventName", "tsz_backup_failure")
    request.add_query_param("EventInfo.1.GroupId", group_id)
    request.add_query_param("EventInfo.1.Content", "tshb-test backup check failed: " + unit)
    response = json.loads(client.do_action_with_exception(request))
    if str(response.get("Code")) != "200":
        raise AlertError("alert_report_failed")
    return {"status": "reported", "unit": unit}


def replica_is_current(backup_config):
    """Check that the latest verified archive has reached the independent replica."""
    replica = backup_config.get("replica")
    if replica is None:
        return True
    if not isinstance(replica, dict):
        return False
    try:
        from backup_archive_oss import connect, identity, read_config, validate_config
        storage = read_config(Path(replica["storage_config"]))
        destination = validate_config({**storage, "endpoint": replica["endpoint"],
                                       "region": replica["region"], "bucket": replica["bucket"]})
        if (destination["bucket"] == storage["bucket"]
                or destination["region"] == storage["region"]):
            return False
        latest = json.loads((Path(backup_config["directory"]) / "latest.json").read_text())
        key = identity(latest["backup_id"], latest["sha256"])
        source = connect(storage).head_object(key)
        target = connect(destination).head_object(key)
        source_crc = source.headers.get("x-oss-hash-crc64ecma")
        target_crc = target.headers.get("x-oss-hash-crc64ecma")
        return (source.content_length == target.content_length == latest["bytes"]
                and bool(source_crc) and source_crc == target_crc
                and target.headers.get("x-oss-server-side-encryption") == "AES256")
    except Exception:
        return False


def heartbeat(config, backup_config, client_factory=None):
    group_id = str(config.get("monitor_group_id", ""))
    if not re.fullmatch(r"[1-9][0-9]*", group_id):
        raise AlertError("invalid_monitor_group")
    if backup_config.get("environment") != "tshb-test":
        raise AlertError("invalid_backup_environment")
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from postgresql_backup_job import inspect_status
    healthy = (inspect_status(backup_config["directory"])["status"] == "passed"
               and shutil.disk_usage(backup_config["directory"]).free >= 5 * 1024 ** 3
               and replica_is_current(backup_config))
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
    request.set_action_name("PutCustomMetric")
    request.set_method("POST")
    request.set_protocol_type("https")
    request.set_accept_format("json")
    request.add_query_param("MetricList.1.MetricName", "tsz_backup_health")
    request.add_query_param("MetricList.1.Type", "0")
    request.add_query_param("MetricList.1.GroupId", group_id)
    request.add_query_param("MetricList.1.Values", json.dumps({"value": int(healthy)}))
    request.add_query_param("MetricList.1.Dimensions", json.dumps({"environment": "tshb-test"}))
    response = json.loads(client.do_action_with_exception(request))
    if str(response.get("Code")) != "200":
        raise AlertError("heartbeat_report_failed")
    return {"status": "reported", "backup_healthy": healthy}


if __name__ == "__main__":
    try:
        if len(sys.argv) == 4 and sys.argv[1] == "heartbeat":
            result = heartbeat(read_config(Path(sys.argv[2])), json.loads(Path(sys.argv[3]).read_text()))
        elif len(sys.argv) == 3:
            result = report(read_config(Path(sys.argv[1])), sys.argv[2])
        else:
            raise AlertError("invalid_arguments")
        print(json.dumps(result))
    except Exception as error:
        code = str(error) if isinstance(error, AlertError) else "alert_report_failed"
        print(json.dumps({"error": code}), file=sys.stderr)
        raise SystemExit(1)
