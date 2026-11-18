#!/usr/bin/env python3
"""Synthetic binary64 reference; no models/audio/weights or upstream imports.
Prompt provenance: c7527577 services/audio-analyzer-clap/analyzer.py PROMPTS.
Run with --upstream <full-history-clone> to regenerate the fixture.
"""
import argparse
import ast
import hashlib
import json
import math
import struct
import pathlib
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--upstream', required=True)
a = p.parse_args()
src = subprocess.check_output(['git', '-C', a.upstream, 'show',
    'c7527577:services/audio-analyzer-clap/analyzer.py'], text=True)
assignments = {n.targets[0].id: ast.literal_eval(n.value) for n in ast.parse(src).body
    if isinstance(n, ast.Assign) and isinstance(n.targets[0], ast.Name)
    and n.targets[0].id in ('PROMPTS', 'VA_CALIB')}
groups = assignments['PROMPTS']
text = ''.join(name + '\n' + '\n'.join(prompts) + '\n' for name, prompts in groups.items())
cases = []
for winner, other in [(0, 0), (.3, .2), (-.4, .1), (1, -1), (-1, 1), (.123, .12)]:
    x, y = math.exp(winner * 15), math.exp(other * 15)
    cases.append([winner, other, x / (x + y)])
platt = [[m, round(1 / (1 + math.exp(-(8.264*m+.249))), 3),
              round(1 / (1 + math.exp(-(10.654*m+.018))), 3)] for m in [-2, -.3, 0, .1, .5, 2]]
def f32(x):
    return struct.unpack('f', struct.pack('f', x))[0]
def vector(seed):
    return [f32(((i*31+seed*17)%101-50)/101) for i in range(512)]
def cosine(a, b):
    return sum(x*y for x,y in zip(a,b))/(math.sqrt(sum(x*x for x in a))*math.sqrt(sum(y*y for y in b)))
synthetic = []
for seed in [1,7,23,49,101]:
    audio = vector(seed)
    sims = [sum(cosine(audio, vector(100+g*3+k)) for k in range(3))/3 for g in range(8)]
    def share(a,b):
        x,y=math.exp(a*15), math.exp(b*15)
        return round(max(0,min(1,x/(x+y))),3)
    synthetic.append(dict(seed=seed,expected=[share(sims[1],sims[0]),share(sims[2],sims[3]),
        round(1/(1+math.exp(-(8.264*(sims[4]-sims[5])+.249))),3),
        round(1/(1+math.exp(-(10.654*(sims[6]-sims[7])+.018))),3)]))
fixture = dict(synthetic=synthetic, provenance='c7527577:services/audio-analyzer-clap/analyzer.py; independent stdlib math transcription',
    source_sha256=hashlib.sha256(src.encode()).hexdigest(), groups=groups,
    prompt_text_sha256=hashlib.sha256(text.encode()).hexdigest(), softmax=cases, platt=platt)
path = pathlib.Path(__file__).resolve().parents[1] / 'crates/kima-vibe/tests/fixtures/zero-shot-reference.json'
path.write_text(json.dumps(fixture, indent=2) + '\n')
print(path, fixture['prompt_text_sha256'])
