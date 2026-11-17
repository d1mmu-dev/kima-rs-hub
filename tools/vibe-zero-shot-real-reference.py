#!/usr/bin/env python3
"""PRIVATE, opt-in real reference. Requires already licensed/local music exports and Python dependencies.
Never downloads/converts models. Audio list: JSON array of exactly 20 {path, license} objects.
Tokenizer: a local roberta-base directory plus an externally supplied trusted manifest
{"repository", "revision", "files": {name: sha256}}; it must equal the revision and file
SHA-256 values the pinned upstream Dockerfile declares, every file is verified before use and
only the verified bytes are loaded (from a private offline cache), never resolved by name.
"""
import argparse
import hashlib
import importlib
import json
import os
from pathlib import Path
import re
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

TOKENIZER_REPOSITORY = 'roberta-base'
HEX40 = re.compile(r'[0-9a-f]{40}')
HEX64 = re.compile(r'[0-9a-f]{64}')
BASENAME = re.compile(r'[A-Za-z0-9_][A-Za-z0-9_.-]*')

class ReferenceRefusal(Exception):
    """A provenance or input check failed. Raised explicitly (never `assert`), so `python -O`
    cannot strip the checks."""

def require(condition, message):
    if not condition:
        raise ReferenceRefusal(message)

def read_small(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, 'rb') as f:
        st = os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode) or st.st_size > limit:
            raise ValueError('expected bounded regular file')
        data = f.read(limit + 1)
        if len(data) > limit:
            raise ValueError('file exceeds limit')
        return data

def upstream_tokenizer_pins(dockerfile):
    """Revision and per-file SHA-256 the pinned upstream image bakes into its offline cache."""
    revisions = set(re.findall(r'huggingface\.co/roberta-base/resolve/([0-9a-f]{40})', dockerfile))
    files = {name: sha for sha, name in
             re.findall(r"echo '([0-9a-f]{64})  '\"\$RS\"'/([A-Za-z0-9_.-]+)'", dockerfile)}
    require(len(revisions) == 1 and files, 'pinned Dockerfile tokenizer pins not found')
    return dict(repository=TOKENIZER_REPOSITORY, revision=revisions.pop(), files=files)

def load_tokenizer_manifest(path):
    manifest = json.loads(read_small(path, 1_000_000))
    require(isinstance(manifest, dict) and set(manifest) == {'repository', 'revision', 'files'},
            'tokenizer manifest must have exactly repository, revision, files')
    require(manifest['repository'] == TOKENIZER_REPOSITORY, 'tokenizer repository')
    require(isinstance(manifest['revision'], str) and HEX40.fullmatch(manifest['revision']),
            'tokenizer revision must be 40 lowercase hex digits')
    files = manifest['files']
    require(isinstance(files, dict) and files, 'tokenizer manifest lists no files')
    for name, sha in files.items():
        require(isinstance(name, str) and BASENAME.fullmatch(name) and name not in ('.', '..'),
                f'tokenizer file name {name!r}')
        require(isinstance(sha, str) and HEX64.fullmatch(sha), f'tokenizer {name} SHA-256')
    return manifest

def stage_tokenizer(manifest, source_dir, cache_root):
    """Copies verified bytes into a private offline Hugging Face cache; returns its HF_HOME."""
    source = Path(source_dir).resolve(strict=True)
    home = Path(cache_root, 'hf')
    repo = home / 'hub' / f'models--{TOKENIZER_REPOSITORY}'
    snapshot = repo / 'snapshots' / manifest['revision']
    snapshot.mkdir(parents=True)
    (repo / 'refs').mkdir()
    (repo / 'refs' / 'main').write_text(manifest['revision'])
    for name, expected in sorted(manifest['files'].items()):
        data = read_small(source / name, 64_000_000)
        require(hashlib.sha256(data).hexdigest() == expected, f'tokenizer {name} SHA-256 mismatch')
        fd = os.open(snapshot / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
    return home

def verify_staged(manifest, home):
    snapshot = home / 'hub' / f'models--{TOKENIZER_REPOSITORY}' / 'snapshots' / manifest['revision']
    require(sorted(os.listdir(snapshot)) == sorted(manifest['files']), 'staged tokenizer files changed')
    for name, expected in manifest['files'].items():
        require(hashlib.sha256(read_small(snapshot / name, 64_000_000)).hexdigest() == expected,
                f'staged tokenizer {name} changed')

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--upstream', required=True)
    p.add_argument('--model-dir', required=True)
    p.add_argument('--audio-list', required=True)
    p.add_argument('--output', required=True)
    p.add_argument('--tokenizer-dir', required=True, help='local roberta-base tokenizer files')
    p.add_argument('--tokenizer-manifest', required=True,
                   help='trusted external JSON: repository, revision, files {name: sha256}')
    p.add_argument('--licensed-private-use-confirmed', action='store_true', required=True)
    args = p.parse_args()
    model = Path(args.model_dir).resolve(strict=True)
    for name, expected in PINS.items():
        require(digest(model / name, 2_000_000_000) == expected, f'{name} SHA-256 mismatch')
    tracks = json.loads(Path(args.audio_list).read_text())
    require(len(tracks) == 20 and len({x['path'] for x in tracks}) == 20,
            'audio list must name exactly 20 distinct files')
    require(all(x['license'].strip() for x in tracks), 'every track needs a licence')
    out = Path(args.output)
    require(not out.exists(), 'refusing to overwrite a private reference')
    def git(*cmd):
        return subprocess.check_output(['git', '-C', args.upstream, *cmd])
    prefix = 'services/audio-analyzer-clap/'
    source = git('show', f'{PIN}:{prefix}analyzer.py')
    tokenizer = load_tokenizer_manifest(args.tokenizer_manifest)
    expected_tokenizer = upstream_tokenizer_pins(git('show', f'{PIN}:{prefix}Dockerfile').decode())
    require(tokenizer == expected_tokenizer, 'tokenizer manifest differs from the pinned upstream image')
    for key in ('TRANSFORMERS_CACHE', 'HF_HUB_CACHE', 'HUGGINGFACE_HUB_CACHE', 'HF_ENDPOINT'):
        os.environ.pop(key, None)
    # Use the exact pinned Python implementation even when the source clone is checked out elsewhere.
    # Only GPL source code is copied; no weights, audio or generated derivatives enter the repo.
    with tempfile.TemporaryDirectory(prefix='kima-zero-shot-reference-') as td:
        hf_home = stage_tokenizer(tokenizer, args.tokenizer_dir, td)
        # Set before the analyzer imports transformers/huggingface_hub: offline, private cache only.
        os.environ.update({'HF_HUB_OFFLINE': '1', 'TRANSFORMERS_OFFLINE': '1',
                           'HF_HOME': str(hf_home), 'HF_HUB_CACHE': str(hf_home / 'hub'),
                           'CLAP_ONNX_MODEL_PATH': str(model / 'music_audio.onnx'),
                           'CLAP_ONNX_TEXT_MODEL_PATH': str(model / 'music_text.onnx')})
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
            require(vector is not None and len(vector) == 512, 'analyzer returned no 512-d vector')
            valence, arousal = analyzer.detect_emotion(vector)
            require(digest(path, 2_000_000_000) == sha, 'audio changed during inference')
            results.append(dict(path=str(path), sha256=sha, license=item['license'],
                audio_vector=vector.tolist(), expected=[analyzer.detect_vocals(vector),
                analyzer.detect_acousticness(vector), valence, arousal]))
        verify_staged(tokenizer, hf_home)
    result = dict(upstream=PIN, analyzer_sha256=hashlib.sha256(source).hexdigest(),
        models=PINS, tokenizer=tokenizer, tracks=results)
    fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as f:
        json.dump(result, f, indent=2)
        f.write('\n')
    print('private reference written; 20 real tracks (not a Rust parity result)')

if __name__ == '__main__':
    main()
