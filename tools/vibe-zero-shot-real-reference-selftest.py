#!/usr/bin/env python3
"""Offline self-test of tools/vibe-zero-shot-real-reference.py's provenance refusals.

No downloads, models or audio: it reads the pinned upstream Dockerfile from a local clone and
exercises the tokenizer manifest/staging checks on synthetic files. Run it both plainly and with
`python3 -O` (which strips `assert`): every refusal must still fire.

    python3 tools/vibe-zero-shot-real-reference-selftest.py --upstream <kima-hub clone>
    python3 -O tools/vibe-zero-shot-real-reference-selftest.py --upstream <kima-hub clone>
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('reference', HERE / 'vibe-zero-shot-real-reference.py')
if spec is None or spec.loader is None:
    raise SystemExit('cannot load tools/vibe-zero-shot-real-reference.py')
ref = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ref)

# The roberta-base revision kima-ml's text tokenizer is pinned to (`text_tokenizer.rs`).
REVISION = 'e2da8e2f811d1448a5b465c236feacd80ffbac7b'
FILES = {'vocab.json', 'merges.txt', 'tokenizer.json', 'tokenizer_config.json'}
# kima-ml's compiled tokenizer.json pin (`text_tokenizer.rs::expected_sha256`).
TOKENIZER_JSON = '847bbeab6174d66a88898f729d52fa8d355fafe1bea101cf960dd404581df70e'


def refuses(fn, *args):
    try:
        fn(*args)
    except (ref.ReferenceRefusal, OSError, ValueError, KeyError):
        return True
    return False


def check(condition, message):
    if not condition:
        raise SystemExit(f'SELFTEST FAILED: {message}')


def manifest_file(directory, value):
    path = Path(directory, f'manifest-{len(os.listdir(directory))}.json')
    path.write_text(json.dumps(value))
    return path


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--upstream', required=True)
    args = p.parse_args()
    dockerfile = subprocess.check_output(
        ['git', '-C', args.upstream, 'show', f'{ref.PIN}:services/audio-analyzer-clap/Dockerfile']
    ).decode()
    pins = ref.upstream_tokenizer_pins(dockerfile)
    check(pins['revision'] == REVISION, 'pinned Dockerfile revision')
    check(set(pins['files']) == FILES, f"pinned Dockerfile files {sorted(pins['files'])}")
    check(pins['files']['tokenizer.json'] == TOKENIZER_JSON, 'tokenizer.json pin agrees with kima-ml')
    check(refuses(ref.upstream_tokenizer_pins, 'no pins here'), 'Dockerfile without pins')

    with tempfile.TemporaryDirectory(prefix='kima-reference-selftest-') as td:
        good = dict(repository='roberta-base', revision=REVISION, files=dict(pins['files']))
        check(ref.load_tokenizer_manifest(manifest_file(td, good)) == pins, 'good manifest loads')
        bad = [
            dict(good, extra=1),
            dict(good, repository='roberta-large'),
            dict(good, revision='E2DA8E2F811D1448A5B465C236FEACD80FFBAC7B'),
            dict(good, revision=REVISION[:39]),
            dict(good, files={}),
            dict(good, files={'..': TOKENIZER_JSON}),
            dict(good, files={'tokenizer.json': TOKENIZER_JSON[:63]}),
            [good],
        ]
        for case in bad:
            check(refuses(ref.load_tokenizer_manifest, manifest_file(td, case)), f'refuse {case!r}')

        source = Path(td, 'source')
        source.mkdir()
        (source / 'tokenizer.json').write_bytes(b'synthetic tokenizer bytes')
        synthetic = dict(repository='roberta-base', revision=REVISION, files={
            'tokenizer.json': hashlib.sha256(b'synthetic tokenizer bytes').hexdigest()})
        home = ref.stage_tokenizer(synthetic, source, Path(td, 'ok'))
        ref.verify_staged(synthetic, home)
        # A trusted pin the local bytes do not match.
        check(refuses(ref.stage_tokenizer, dict(synthetic, files={'tokenizer.json': TOKENIZER_JSON}),
                      source, Path(td, 'mismatch')), 'SHA-256 mismatch refused')
        # Extra file in the staged snapshot after verification.
        (home / 'hub' / 'models--roberta-base' / 'snapshots' / REVISION / 'extra').write_bytes(b'x')
        check(refuses(ref.verify_staged, synthetic, home), 'extra staged file refused')
        # A symlinked source file is never followed.
        linked = Path(td, 'linked')
        linked.mkdir()
        (linked / 'tokenizer.json').symlink_to(source / 'tokenizer.json')
        check(refuses(ref.stage_tokenizer, synthetic, linked, Path(td, 'symlink')), 'symlink refused')
        # The pinned upstream manifest against these synthetic bytes: mismatch.
        check(refuses(ref.stage_tokenizer, pins, source, Path(td, 'upstream')), 'missing upstream files refused')
    print(f'selftest ok (python optimize={sys.flags.optimize})')


if __name__ == '__main__':
    main()
