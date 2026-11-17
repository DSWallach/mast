#!/usr/bin/python3
"""Entry guard for a process already inside the driver sandbox."""
import errno
import os
import pwd
import shutil
import socket
import sys


def fail(message):
    print("driver startup refused: " + message, file=sys.stderr)
    sys.exit(1)


def directory(name):
    value = os.environ.get(name)
    if not value or not os.path.isabs(value) or not os.path.isdir(value):
        fail(name + " must name an existing absolute directory")
    return os.path.realpath(value)


def inside(path, parent):
    return os.path.commonpath((path, parent)) == parent


def validate_paths(root, paths, account, temporary_root):
    # Resolve all inputs here too, so callers cannot accidentally skip the
    # symlink check when testing or reusing this validation.
    root = os.path.realpath(root)
    paths = [os.path.realpath(path) for path in paths]
    account = os.path.realpath(account)
    if not inside(root, temporary_root) or not os.path.basename(root).startswith("mast-drive."):
        return "scratch root is invalid"
    if inside(root, account) or inside(account, root):
        return "scratch root overlaps account home"
    if any(path == root or not inside(path, root) or inside(path, account) for path in paths):
        return "environment directory is outside scratch root"
    for index, path in enumerate(paths):
        if any(inside(path, other) or inside(other, path) for other in paths[index + 1:]):
            return "environment directories overlap"
    return None


def main():
    account = os.path.realpath(pwd.getpwuid(os.getuid()).pw_dir)
    if directory("MAST_DRIVER_ACCOUNT_HOME") != account:
        fail("account home does not match the current user")
    root = directory("MAST_DRIVER_ROOT")
    paths = [directory(name) for name in ("HOME", "MAST_DATA_DIR", "TMPDIR")]
    error = validate_paths(root, paths, account, os.path.realpath("/private/tmp"))
    if error:
        fail(error)
    git = shutil.which("git")
    expected_git = os.path.join(root, "shims", "git")
    if not git or os.path.realpath(git) != expected_git:
        fail("git does not resolve to the scratch shim")
    if len(sys.argv) < 2 or not os.path.isabs(sys.argv[1]):
        fail("absolute executable path is required")
    # This must be an explicit sandbox denial. A timeout or routing failure
    # would also happen without a sandbox and therefore cannot prove isolation.
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
            connection.settimeout(1)
            connection.connect(("192.0.2.1", 443))
    except OSError as error:
        if error.errno not in (errno.EPERM, errno.EACCES):
            fail("network probe did not receive a sandbox denial")
    else:
        fail("outbound network is available")
    os.execv(sys.argv[1], sys.argv[1:])


if __name__ == "__main__":
    main()
