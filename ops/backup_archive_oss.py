#!/usr/bin/env python3
"""Archive and retrieve one scheduled backup in a dedicated private OSS bucket."""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys


class ArchiveError(Exception):
    pass


def identity(backup_id, digest):
    if (not re.fullmatch(r"scheduled-[A-Za-z0-9_-]+", backup_id)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)):
        raise ArchiveError("invalid_backup_identity")
    return "database-backups/" + backup_id + "/backup.dump"


def read_config(path):
    path = Path(path)
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
            or info.st_mode & 0o077):
        raise ArchiveError("insecure_storage_config")
    config = json.loads(path.read_text())
    endpoint, region, bucket = (config.get(k, "") for k in ("endpoint", "region", "bucket"))
    if (not re.fullmatch(r"https://oss-[a-z0-9-]+(?:-internal)?\.aliyuncs\.com", endpoint)
            or not re.fullmatch(r"[a-z0-9-]+", region)
            or not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,61}[a-z0-9]", bucket)
            or not config.get("access_key_id") or not config.get("access_key_secret")):
        raise ArchiveError("invalid_storage_config")
    return config


def connect(config):
    import oss2
    auth = oss2.AuthV4(config["access_key_id"], config["access_key_secret"])
    bucket = oss2.Bucket(auth, config["endpoint"], config["bucket"],
                         region=config["region"], connect_timeout=15)
    if bucket.get_bucket_acl().acl != "private":
        raise ArchiveError("bucket_not_private")
    if bucket.get_bucket_public_access_block().block_public_access is not True:
        raise ArchiveError("public_access_not_blocked")
    # OSS ignores forbid-overwrite for versioned buckets; do not silently weaken it.
    if bucket.get_bucket_versioning().status not in (None, ""):
        raise ArchiveError("bucket_versioning_unsupported")
    return bucket


def digest_stream(stream, output=None):
    digest, size = hashlib.sha256(), 0
    try:
        while True:
            block = stream.read(1024 * 1024)
            if not block:
                break
            digest.update(block)
            size += len(block)
            if output is not None:
                output.write(block)
    finally:
        stream.close()
    return digest.hexdigest(), size


def archive(bucket, bucket_name, file_path, expected):
    file = Path(file_path)
    key = identity(file.parent.name, expected)
    if file.name != "backup.dump" or not file.is_file() or file.is_symlink():
        raise ArchiveError("invalid_backup_file")
    local_digest, size = digest_stream(file.open("rb"))
    if local_digest != expected or size == 0:
        raise ArchiveError("local_backup_mismatch")
    if size > 5 * 1024 ** 3:
        raise ArchiveError("backup_exceeds_single_upload_limit")
    with file.open("rb") as source:
        result = bucket.put_object(key, source, headers={
            "x-oss-forbid-overwrite": "true",
            "x-oss-server-side-encryption": "AES256",
            "x-oss-object-acl": "private",
        })
    if result.status != 200:
        raise ArchiveError("upload_failed")
    remote = bucket.get_object(key)
    if remote.headers.get("x-oss-server-side-encryption") != "AES256":
        remote.close()
        raise ArchiveError("remote_encryption_unverified")
    remote_digest, remote_size = digest_stream(remote)
    if (remote_digest, remote_size) != (expected, size):
        raise ArchiveError("remote_backup_mismatch")
    return {"verified": True, "sha256": expected, "bytes": size,
            "reference": "oss:" + bucket_name + ":" + key}


def fetch(bucket, backup_id, expected, destination):
    key = identity(backup_id, expected)
    destination = Path(destination)
    # Exclusive create prevents replacing another file or following a pre-existing symlink.
    try:
        fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        raise ArchiveError("destination_exists") from None
    try:
        with os.fdopen(fd, "wb") as output:
            stream = bucket.get_object(key)
            if stream.headers.get("x-oss-server-side-encryption") != "AES256":
                stream.close()
                raise ArchiveError("remote_encryption_unverified")
            actual, size = digest_stream(stream, output)
            output.flush()
            os.fsync(output.fileno())
        if actual != expected or size == 0:
            raise ArchiveError("retrieved_backup_mismatch")
    except BaseException:
        destination.unlink(missing_ok=True)
        raise
    return {"retrieved": True}


def main(args):
    if len(args) == 3:
        config_path, path, expected = args
        config = read_config(config_path)
        return archive(connect(config), config["bucket"], path, expected)
    if len(args) == 5 and args[0] == "fetch":
        _, config_path, backup_id, expected, destination = args
        return fetch(connect(read_config(config_path)), backup_id, expected, destination)
    raise ArchiveError("invalid_arguments")


if __name__ == "__main__":
    try:
        print(json.dumps(main(sys.argv[1:])))
    except Exception as error:
        # SDK exceptions may contain credentials, signed URLs or provider responses.
        code = str(error) if isinstance(error, ArchiveError) else "storage_operation_failed"
        print(json.dumps({"error": code}), file=sys.stderr)
        raise SystemExit(1)
