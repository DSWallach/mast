#!/usr/bin/env python3
"""Check risky test references against individually recorded exceptions."""

from collections import Counter
import json
from pathlib import Path
import re
import sys
try:
    import tomllib
except ModuleNotFoundError:
    sys.exit("check_test_isolation.py requires Python 3.11 or newer")

ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts/test_isolation_allowlist.jsonl"
RISK = re.compile(
    r'dirs::home_dir|\bhome_dir\s*\(|\$HOME|\.zeron|\.claude|\.codex|'
    r'CLAUDE_CONFIG_DIR|CODEX_HOME|(?:MAST|ZERON)_[A-Z_]*(?:DATA|DIR)[A-Z_]*|'
    r'tempdir_in\s*\(|TempDir::new_in\s*\(|["\']HOME["\']|'
    r'passwd_entry\s*\(|getpwuid|getpwnam|/Users/|'
    r'data_dir.{0,80}(?:unwrap_or|default)|(?:unwrap_or|default).{0,80}data_dir'
)


def sources():
    for tree in (ROOT / "crates", ROOT / "apps"):
        for path in sorted(tree.rglob("*.rs")):
            parts = path.relative_to(ROOT).parts
            if "target" in parts or "target-clean" in parts:
                continue
            # Scan shared production files too: test helpers are often mixed
            # into ordinary modules and do not reliably carry a test attribute.
            yield path, path.read_text()


def findings():
    for path, contents in sources():
        counts = Counter()
        for number, line in enumerate(contents.splitlines(), 1):
            value = line.strip()
            if not value or value.startswith(("//", "*")) or not RISK.search(value):
                continue
            counts[value] += 1
            yield (str(path.relative_to(ROOT)), value, counts[value]), number


def test_binaries():
    for tree in (ROOT / "crates", ROOT / "apps"):
        for manifest in sorted(tree.glob("*/Cargo.toml")):
            package = manifest.parent
            metadata = tomllib.loads(manifest.read_text())
            for entry in [package / "src/lib.rs", package / "src/main.rs"]:
                if entry.is_file():
                    yield entry
            yield from sorted((package / "src/bin").glob("*.rs"))
            if metadata.get("package", {}).get("autotests", True):
                yield from sorted((package / "tests").glob("*.rs"))
            for target in metadata.get("test", []):
                path = package / target.get("path", f'tests/{target["name"]}.rs')
                if path.is_file():
                    yield path
            # Explicit binary targets may live outside src/bin (for example,
            # a native test fixture compiled as a binary target).
            for block in re.split(r'^\[\[bin\]\]\s*$', manifest.read_text(), flags=re.MULTILINE)[1:]:
                block = re.split(r'^\[', block, maxsplit=1, flags=re.MULTILINE)[0]
                match = re.search(r'^path\s*=\s*"([^"]+)"', block, flags=re.MULTILINE)
                if match and (package / match.group(1)).is_file():
                    yield package / match.group(1)


def main():
    permitted = {}
    if ALLOWLIST.exists():
        for number, line in enumerate(ALLOWLIST.read_text().splitlines(), 1):
            entry = json.loads(line)
            key = (entry["path"], entry["line"], entry["occurrence"])
            if not entry.get("reason", "").strip() or key in permitted:
                raise ValueError(f"invalid allowlist entry at line {number}")
            permitted[key] = entry["reason"]
    failed = False
    # With automatic discovery disabled, a new file must be registered in the
    # consolidated suite or Cargo will silently omit its tests.
    engine_tests = ROOT / "crates/engine/tests"
    suite = engine_tests / "it/main.rs"
    if suite.exists():
        registered = {
            (suite.parent / name).resolve()
            for name in re.findall(r'#\[path = "([^"]+)"\]', suite.read_text())
        }
        # This macro fixture is included by the sessions unit-test module.
        included_unit_fixtures = {"session_publication_bench.rs"}
        for path in sorted(engine_tests.glob("*.rs")):
            if path.name not in included_unit_fixtures and path.resolve() not in registered:
                print(f"{path.relative_to(ROOT)}: missing from engine integration suite")
                failed = True
    found = set()
    for key, number in findings():
        found.add(key)
        if key not in permitted:
            print(f"{key[0]}:{number}: unreviewed home reference: {key[1]}")
            failed = True
    for path in test_binaries():
        if 'test_support/isolation.rs' not in path.read_text():
            print(f"{path.relative_to(ROOT)}: missing test isolation initializer")
            failed = True
    for key in sorted(permitted.keys() - found):
        print(f"{key[0]}: obsolete allowlist entry: {key[1]}")
        failed = True
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
