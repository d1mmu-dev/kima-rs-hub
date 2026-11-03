#!/usr/bin/env python3
"""Guard the explicit CI/image native runtime packaging contract."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class PackagingTests(unittest.TestCase):
    def test_bad_wheel_fails_before_creating_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            wheel = root / 'bad.whl'
            wheel.write_bytes(b'corrupt package')
            result = subprocess.run(['python3', str(ROOT / 'scripts/provision-ort.py'),
                                     str(wheel), str(root / 'ort')], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('wheel SHA-256 mismatch', result.stderr)
            self.assertFalse((root / 'ort').exists())

    def test_unsupported_platform_fails_before_downloading(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            uname = root / 'uname'
            uname.write_text('#!/bin/sh\nprintf "unsupported\\n"\n')
            uname.chmod(0o755)
            result = subprocess.run(['sh', str(ROOT / 'scripts/acquire-ort.sh'), str(root / 'ort')],
                                    env={**os.environ, 'PATH': str(root) + ':' + os.environ['PATH']},
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('supports only linux/amd64', result.stderr)
            self.assertNotIn('curl', result.stderr)
            self.assertFalse((root / 'ort').exists())

    def test_ci_provisions_before_rust_tests(self):
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn('ORT_DYLIB_PATH: /opt/kima/ort/libonnxruntime.so.1.22.0', ci)
        self.assertLess(ci.index('scripts/acquire-ort.sh /opt/kima/ort'), ci.index('cargo test --workspace'))

    def test_final_image_has_trusted_runtime_and_notices(self):
        docker = (ROOT / 'Dockerfile').read_text()
        self.assertIn('scripts/acquire-ort.sh /opt/kima/ort', docker)
        self.assertIn('COPY --from=ort-runtime /opt/kima/ort /opt/kima/ort', docker)
        self.assertIn('ORT_DYLIB_PATH=/opt/kima/ort/libonnxruntime.so.1.22.0', docker)
        self.assertIn('chmod -R a-w /opt/kima/ort', docker)


if __name__ == '__main__':
    unittest.main()
