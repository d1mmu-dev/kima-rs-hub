"""Capture selection/preflight tests; never start Docker or manufacture API responses."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
NAMES = ('login me list create update switch me_switched settings settings_switched '
         'playlist playlist_switched delete list_after_delete').split()
EXPECTED = {f'profiles_{name}_response.json' for name in NAMES} | {'profiles_capture_manifest.json'}


class ProfilesCapture(unittest.TestCase):
    def test_dirty_and_untracked_targets_refused_before_docker(self):
        # A disposable Git tree, not the real fixture directory. No database is started.
        with tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR')) as directory:
            root = Path(directory)
            (root / 'tools').mkdir()
            (root / 'tools/capture-api-fixtures.sh').write_bytes(
                (ROOT / 'tools/capture-api-fixtures.sh').read_bytes())
            fixture = root / 'crates/kima-api-types/tests/fixtures/profiles_me_response.json'
            fixture.parent.mkdir(parents=True)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            fixture.write_text('protected untracked content')
            env = {**os.environ, 'KIMA_CAPTURE_SET': 'profiles'}
            env.pop('DATABASE_URL', None)
            env.pop('PGDATABASE', None)
            binaries = root / 'bin'
            binaries.mkdir()
            for command in ('docker', 'ffmpeg', 'jq', 'curl', 'psql', 'python3', 'cargo'):
                executable = binaries / command
                executable.write_text('#!/bin/sh\necho unexpected-execution >&2\nexit 99\n')
                executable.chmod(0o755)
            env['PATH'] = str(binaries) + os.pathsep + env['PATH']
            for tracked in (False, True):
                if tracked:
                    subprocess.run(['git', 'add', '.'], cwd=root, check=True)
                    subprocess.run(['git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                                    'commit', '-qm', 'fixture'], cwd=root, check=True)
                    fixture.write_text('protected dirty content')
                result = subprocess.run(['bash', 'tools/capture-api-fixtures.sh'], cwd=root,
                                        env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn('Uncommitted or untracked fixtures', result.stderr)
                self.assertNotIn('unexpected-execution', result.stderr)
                self.assertIn('protected', fixture.read_text())

    def test_external_database_refused_even_in_list_mode(self):
        env = {**os.environ, 'KIMA_CAPTURE_SET': 'profiles', 'DATABASE_URL': 'forbidden'}
        result = subprocess.run(['bash', 'tools/capture-api-fixtures.sh', '--list'],
                                cwd=ROOT, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('No external', result.stderr)

    def test_profile_capture_implementation_is_present_and_valid_shell(self):
        source = ROOT / 'tools/capture-profile-fixtures.inc.sh'
        self.assertTrue(source.is_file(), 'selected profiles set must have a runnable capture')
        result = subprocess.run(['bash', '-n', str(source)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_profiles_selection_is_exact_and_disjoint(self):
        env = {**os.environ, 'KIMA_CAPTURE_SET': 'profiles'}
        env.pop('DATABASE_URL', None)
        env.pop('PGDATABASE', None)
        result = subprocess.run(['bash', 'tools/capture-api-fixtures.sh', '--list'],
                                cwd=ROOT, env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(result.stdout.splitlines()), EXPECTED)
        self.assertEqual(len(result.stdout.splitlines()), len(EXPECTED))


if __name__ == '__main__':
    unittest.main()
