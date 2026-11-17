#!/usr/bin/env python3
"""PRIVATE, opt-in real reference. Requires already licensed/local music exports and Python dependencies.
Never downloads/converts models. Audio list: JSON array of exactly 20 {path, license} objects.
"""
import argparse
import hashlib
import importlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile

PIN = 'c7527577d925b20f5adf410c7fd4e0c8ac250f69'
PINS = {'music_audio.onnx': 'ab1bf5eba067b246198c630f153f774e8f1c3169b3924d4d6a4dc247bd73de97',
        'music_text.onnx': '6dbba074b27f8973d84c5c7ae9acd0b9139a6cd9dd82bea6e87954dc3b835956'}

def digest(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, 'rb') as f:
        st = os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode) or st.st_size > limit:
            raise ValueError('expected bounded regular file')
        h = hashlib.sha256()
        total = 0
        while chunk := f.read(1024 * 1024):
            total += len(chunk)
            if total > limit:
                raise ValueError('file exceeds limit')
            h.update(chunk)
        return h.hexdigest()

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--upstream', required=True)
    p.add_argument('--model-dir', required=True)
    p.add_argument('--audio-list', required=True)
    p.add_argument('--output', required=True)
    p.add_argument('--licensed-private-use-confirmed', action='store_true', required=True)
    args = p.parse_args()
    model = Path(args.model_dir).resolve(strict=True)
    for name, expected in PINS.items():
        assert digest(model / name, 2_000_000_000) == expected, name
    tracks = json.loads(Path(args.audio_list).read_text())
    assert len(tracks) == 20 and len({x['path'] for x in tracks}) == 20
    assert all(x['license'].strip() for x in tracks)
    out = Path(args.output)
    assert not out.exists(), 'refusing to overwrite a private reference'
    env = {'HF_HUB_OFFLINE': '1', 'TRANSFORMERS_OFFLINE': '1',
           'CLAP_ONNX_MODEL_PATH': str(model / 'music_audio.onnx'),
           'CLAP_ONNX_TEXT_MODEL_PATH': str(model / 'music_text.onnx')}
    os.environ.update(env)
    def git(*cmd):
        return subprocess.check_output(['git', '-C', args.upstream, *cmd])
    prefix = 'services/audio-analyzer-clap/'
    source = git('show', f'{PIN}:{prefix}analyzer.py')
    # Use the exact pinned Python implementation even when the source clone is checked out elsewhere.
    # Only GPL source code is copied; no weights, audio or generated derivatives enter the repo.
    with tempfile.TemporaryDirectory(prefix='kima-zero-shot-reference-') as td:
        for raw in git('ls-tree', '-r', '--name-only', PIN, '--', prefix).decode().splitlines():
            relative = raw.removeprefix(prefix)
            if '/' not in relative and relative.endswith('.py'):
                Path(td, relative).write_bytes(git('show', f'{PIN}:{raw}'))
        sys.path.insert(0, td)
        analyzer = importlib.import_module('analyzer').CLAPAnalyzer()
        results = []
        for item in tracks:
            path = Path(item['path']).resolve(strict=True)
            sha = digest(path, 2_000_000_000)
            vector = analyzer.get_audio_embedding(str(path))
            assert vector is not None and len(vector) == 512
            valence, arousal = analyzer.detect_emotion(vector)
            assert digest(path, 2_000_000_000) == sha, 'audio changed during inference'
            results.append(dict(path=str(path), sha256=sha, license=item['license'],
                audio_vector=vector.tolist(), expected=[analyzer.detect_vocals(vector),
                analyzer.detect_acousticness(vector), valence, arousal]))
    result = dict(upstream=PIN, analyzer_sha256=hashlib.sha256(source).hexdigest(),
        models=PINS, tracks=results)
    fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as f:
        json.dump(result, f, indent=2)
        f.write('\n')
    print('private reference written; 20 real tracks (not a Rust parity result)')

if __name__ == '__main__':
    main()
