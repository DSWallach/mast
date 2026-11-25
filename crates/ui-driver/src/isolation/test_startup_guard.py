"""Path guard checks use only fake account directories under mktemp roots."""
import os
import shutil
import subprocess
import unittest

import startup_guard


class StartupGuardTest(unittest.TestCase):
    def setUp(self):
        self.root = subprocess.check_output(
            ["/usr/bin/mktemp", "-d", "/tmp/mast-drive.XXXXXX"], text=True
        ).strip()
        self.account = subprocess.check_output(
            ["/usr/bin/mktemp", "-d", "/tmp/mast-account.XXXXXX"], text=True
        ).strip()
        self.paths = [os.path.join(self.root, name) for name in ("home", "data", "tmp")]
        for path in self.paths:
            os.mkdir(path)
        for name in (".zeron", ".mast", ".claude", ".codex"):
            os.mkdir(os.path.join(self.account, name))
        self.temporary_root = os.path.realpath("/tmp")

    def tearDown(self):
        shutil.rmtree(self.root)
        shutil.rmtree(self.account)

    def validate(self, paths):
        return startup_guard.validate_paths(self.root, paths, self.account, self.temporary_root)

    def test_refuses_account_locations_and_aliases(self):
        self.assertIsNone(self.validate(self.paths))
        for name in (".zeron", ".mast", ".claude", ".codex"):
            protected = os.path.join(self.account, name)
            alias = os.path.join(self.root, "alias-" + name)
            os.symlink(protected, alias)
            for index in range(3):
                for unsafe in (protected, alias, self.account):
                    paths = list(self.paths)
                    paths[index] = unsafe
                    self.assertIsNotNone(self.validate(paths))

    def test_refuses_overlapping_directories(self):
        paths = list(self.paths)
        paths[1] = paths[0]
        self.assertIsNotNone(self.validate(paths))


if __name__ == "__main__":
    unittest.main()
