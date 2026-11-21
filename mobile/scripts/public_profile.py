import ipaddress
import json
import os
import re
import stat
import sys
import unicodedata


def unique_fields(pairs):
    fields = {}
    for key, value in pairs:
        if key in fields:
            raise ValueError("duplicate field")
        fields[key] = value
    return fields


def valid_endpoint(endpoint):
    host, _, port = endpoint.rpartition(":")
    if not re.fullmatch(r"[0-9]{1,5}", port) or not 1 <= int(port) <= 65535:
        return False
    if host.startswith("[") and host.endswith("]"):
        literal, percent, scope = host[1:-1].partition("%")
        if percent and (
            not re.fullmatch(r"[0-9]{1,10}", scope) or int(scope) > 0xFFFFFFFF
        ):
            return False
        address = ipaddress.IPv6Address(literal)
    else:
        address = ipaddress.IPv4Address(host)
    return not address.is_unspecified and not address.is_multicast


def read_profile(path):
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    directory, filename = os.path.split(path)
    if filename in ("", ".", ".."):
        raise ValueError("expected a profile filename")
    # The operator chooses the directory. Anchor the file open to its descriptor
    # so subsequent renames cannot redirect it through a different directory.
    parent = os.open(directory or ".", os.O_DIRECTORY | os.O_RDONLY | os.O_NOFOLLOW)
    try:
        fd = os.open(filename, flags, dir_fd=parent)
    finally:
        os.close(parent)
    with os.fdopen(fd, "rb") as source:
        metadata = os.fstat(source.fileno())
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid not in (os.geteuid(), 0)
            or metadata.st_mode & 0o022
        ):
            raise ValueError("unsafe file")
        maximum = 16 * 1024
        contents = source.read(maximum + 1)
        if len(contents) > maximum:
            raise ValueError("file too large")
    fields = {"version", "label", "endpoint", "server_node_id", "server_public_key_hex"}
    profile = json.loads(contents.decode("utf-8"), object_pairs_hook=unique_fields)
    if (
        not isinstance(profile, dict)
        or profile.keys() != fields
        or type(profile["version"]) is not int
        or profile["version"] != 1
        or any(not isinstance(profile[field], str) for field in fields - {"version"})
    ):
        raise ValueError("invalid public profile schema")
    label = profile["label"]
    if (
        not label.strip()
        or len(label.encode("utf-8")) > 128
        or any(unicodedata.category(char) == "Cc" for char in label)
        or not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", profile["server_node_id"])
        or not re.fullmatch(r"[A-Fa-f0-9]{3904}", profile["server_public_key_hex"])
        or not valid_endpoint(profile["endpoint"])
    ):
        raise ValueError("invalid public profile fields")
    return profile


if __name__ == "__main__":
    try:
        print(json.dumps(read_profile(sys.argv[1]), ensure_ascii=False))
    except (OSError, ValueError, RecursionError):
        sys.exit("expected a valid v1 public server profile in a trusted regular file")
