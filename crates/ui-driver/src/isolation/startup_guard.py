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
    if os.path.dirname(root) != temporary_root or not os.path.basename(root).startswith("mast-drive."):
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
    linux = sys.platform.startswith("linux")
    account = directory("MAST_DRIVER_ACCOUNT_HOME")
    if linux:
        # Bubblewrap presents an empty directory in place of the account home.
        if os.listdir(account):
            fail("real account home is visible")
        host = os.environ.get("MAST_SOAK_HOST_NETNS", "")
        current = os.stat("/proc/self/ns/net").st_ino
        if not host.isdecimal() or int(host) == current:
            fail("a separate network namespace is required")
        with open("/proc/net/dev", encoding="ascii") as interfaces:
            if any(line.split(":")[0].strip() != "lo" for line in interfaces if ":" in line):
                fail("the network namespace has external interfaces")
    elif account != os.path.realpath(pwd.getpwuid(os.getuid()).pw_dir):
        fail("account home does not match the current user")
    root = directory("MAST_DRIVER_ROOT")
    paths = [directory(name) for name in ("HOME", "MAST_DATA_DIR", "TMPDIR")]
    error = validate_paths(root, paths, account, os.path.realpath("/tmp"))
    if error:
        fail(error)
    git = shutil.which("git")
    expected_git = os.path.join(root, "shims", "git")
    if not git or os.path.realpath(git) != expected_git:
        fail("git does not resolve to the scratch shim")
    if len(sys.argv) < 2 or not os.path.isabs(sys.argv[1]):
        fail("absolute executable path is required")
    # A namespace with only loopback returns ENETUNREACH. On macOS the
    # seatbelt policy must explicitly deny the connection.
    denied = (errno.ENETUNREACH, errno.EPERM, errno.EACCES) if linux else (errno.EPERM, errno.EACCES)
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
            connection.settimeout(1)
            connection.connect(("192.0.2.1", 443))
    except OSError as error:
        if error.errno not in denied:
            fail("network probe did not receive a sandbox denial")
    else:
        fail("outbound network is available")
    os.execv(sys.argv[1], sys.argv[1:])


if __name__ == "__main__":
    main()
