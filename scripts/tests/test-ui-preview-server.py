"""Exercise the persistent server from a different cwd without Cargo or a GPU."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


class PreviewServerTest(unittest.TestCase):
    def test_reuses_renderer_until_checkout_provenance_changes(self):
        source = Path(__file__).resolve().parents[1] / "ui-preview-server.py"
        with tempfile.TemporaryDirectory() as scratch:
            scratch = Path(scratch).resolve()
            root = scratch / "checkout"
            scripts = root / "scripts"
            scripts.mkdir(parents=True)
            server = scripts / source.name
            shutil.copyfile(source, server)
            (scripts / "build-revision.sh").write_text("cat revision\n")
            (root / "revision").write_text("a" * 40 + "\n")
            (root / "branch").write_text("first\n")
            fake_bin = root / "bin"
            fake_bin.mkdir()
            self.executable(fake_bin / "git", "#!/bin/sh\ncat branch\n")
            self.executable(fake_bin / "cargo", """#!/bin/sh
printf '{"reason":"compiler-artifact","target":{"name":"ui_preview"},"executable":"%s/renderer"}\n' "$PWD"
""")
            self.executable(root / "renderer", f"""#!{sys.executable}
import json, os, sys
for line in sys.stdin:
    print("MAST_PREVIEW_RESPONSE=" + json.dumps({{
        "cwd": os.getcwd(),
        "warm": os.environ["MAST_PREVIEW_WARM"],
        "serve": os.environ["MAST_PREVIEW_SERVE"],
        "command": os.environ["MAST_UI_COMMAND"],
        "request": json.loads(line),
    }}), flush=True)
""")
            protocol = scratch / "protocol"
            protocol.mkdir()
            env = dict(os.environ, PATH=str(fake_bin) + os.pathsep + os.environ["PATH"])
            process = subprocess.Popen(
                [sys.executable, str(server), str(protocol)],
                cwd=scratch, env=env, stderr=subprocess.PIPE, text=True,
            )
            try:
                first = self.request(process, protocol, {"lightOnly": True})
                self.assertEqual(first["cwd"], str(root))
                self.assertEqual((first["warm"], first["serve"]), ("1", "1"))
                self.assertTrue(first["command"].startswith("scripts/ui-preview.sh --serve "))
                self.assertEqual(first["request"], {"lightOnly": True})
                same = self.request(process, protocol, {})
                self.assertEqual(same["rendererPid"], first["rendererPid"])

                (root / "branch").write_text("second\n")
                branched = self.request(process, protocol, {})
                self.assertNotEqual(branched["rendererPid"], same["rendererPid"])

                (root / "revision").write_text("b" * 40 + "-dirty\n")
                revised = self.request(process, protocol, {})
                self.assertNotEqual(revised["rendererPid"], branched["rendererPid"])
            finally:
                (protocol / "shutdown").touch()
                try:
                    process.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate()

    @staticmethod
    def executable(path, source):
        path.write_text(source)
        path.chmod(0o700)

    def request(self, process, protocol, request):
        response = protocol / "response.json"
        response.unlink(missing_ok=True)
        draft = protocol / "draft.json"
        draft.write_text(json.dumps(request))
        draft.replace(protocol / "request.json")
        deadline = time.monotonic() + 5
        while not response.exists() and process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(response.exists(), "server did not respond")
        payload = json.loads(response.read_text())
        self.assertNotIn("error", payload)
        return payload


if __name__ == "__main__":
    unittest.main()
