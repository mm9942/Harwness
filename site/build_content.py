#!/usr/bin/env python3
"""Build site/dist from the hand-maintained sources in site/src.

Run with: python3 -I site/build_content.py   (or `make site`)

The pages in site/src are the source of truth (they were hand-edited after
the original v0.3.0 generator ran; that generator is kept for reference in
site/legacy/build_content_v030.py and must NOT be used to build the site).
dist/ is wiped and regenerated on every run and is git-ignored.
"""
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parent
SRC = ROOT / 'src'
DIST = ROOT / 'dist'

if not SRC.is_dir():
    raise SystemExit(f'missing source directory: {SRC}')
if DIST.exists():
    shutil.rmtree(DIST)
shutil.copytree(SRC, DIST)
n = sum(1 for p in DIST.rglob('*') if p.is_file())
print(f'built {n} files into {DIST.relative_to(ROOT.parent)}')
