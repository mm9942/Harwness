#!/usr/bin/env python3
"""Prepare one immutable GitHub/R2 bundle; never repair received checksums."""
import argparse
import hashlib
import io
from pathlib import Path
import re
import subprocess
import tarfile

LINUX_TARGETS = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu')
BINARIES = {'harw', 'killer', 'harw-job-exec', 'harw-agent-runner'}
DOCUMENTS = {'README.md', 'LICENSE-MIT', 'LICENSE-APACHE'}


def validate_tag(tag):
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+', tag):
        raise ValueError('Expected stable vX.Y.Z tag')


def package_source(tag, dist, root):
    validate_tag(tag)
    # git archive uses pax metadata (including a global commit comment).
    # Re-encode as strict ustar to match the release archive contract.
    data = subprocess.check_output(['git', '-C', str(root), 'archive', '--format=tar',
                                    f'--prefix=harwness-{tag[1:]}/', 'HEAD'])
    dist.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as source, tarfile.open(
        dist / f'harwness-{tag[1:]}-source.tar.gz', 'w:gz', format=tarfile.USTAR_FORMAT
    ) as output:
        for member in source:
            if not (member.isfile() or member.isdir()):
                raise ValueError(f'Source contains unsupported entry: {member.name}')
            member.pax_headers = {}
            output.addfile(member, source.extractfile(member) if member.isfile() else None)


def validate_archive(path, root, required):
    with tarfile.open(path) as archive:
        names = set()
        total = 0
        for member in archive:
            parts = member.name.removesuffix('/').split('/')
            if (not parts or parts[0] != root or '..' in parts or
                    '.' in parts or '' in parts or
                    not (member.isfile() or member.isdir())):
                raise ValueError(f'Unsafe archive member: {member.name}')
            if member.name in names:
                raise ValueError(f'Duplicate archive member: {member.name}')
            names.add(member.name)
            total += member.size
            if total > 512 << 20:
                raise ValueError('Archive exceeds installer size limit')
            if member.name.removeprefix(root + '/') in BINARIES and not member.mode & 0o111:
                raise ValueError(f'Non-executable binary: {member.name}')
        for name in required:
            member = archive.getmember(f'{root}/{name}') if f'{root}/{name}' in names else None
            if member is None or not member.isfile():
                raise ValueError(f'Archive missing regular file: {root}/{name}')


def verify_bundle(tag, dist):
    validate_tag(tag)
    expected = {f'harw-{tag}-{target}.tar.gz' for target in LINUX_TARGETS}
    source = f'harwness-{tag[1:]}-source.tar.gz'
    expected.add(source)
    actual = {p.name for p in dist.glob('*.tar.gz')}
    # Android is not promoted until native Termux execution is an enforced gate.
    if actual != expected:
        raise ValueError(f'Incomplete/unverified asset set: expected {sorted(expected)}, got {sorted(actual)}')
    sums = {}
    for line in (dist / 'SHA256SUMS').read_text().splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  ([^/\s]+)', line)
        if not match or match[2] in sums:
            raise ValueError('Malformed or duplicate SHA256SUMS entry')
        sums[match[2]] = match[1]
    if set(sums) != expected:
        raise ValueError('SHA256SUMS does not cover exactly the release assets')
    for name in sorted(expected):
        if hashlib.sha256((dist / name).read_bytes()).hexdigest() != sums[name]:
            raise ValueError(f'Checksum mismatch: {name}')
        if name == source:
            validate_archive(dist / name, f'harwness-{tag[1:]}',
                             {'Cargo.toml', 'Cargo.lock', 'Makefile', 'scripts/install.sh'})
        else:
            validate_archive(dist / name, name.removesuffix('.tar.gz'), BINARIES | DOCUMENTS)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('prepare', 'verify'))
    parser.add_argument('tag')
    parser.add_argument('dist', type=Path)
    args = parser.parse_args()
    if args.command == 'prepare':
        # Sidecars came from the build jobs. Verify before combining them.
        for target in LINUX_TARGETS:
            name = f'harw-{args.tag}-{target}.tar.gz'
            recorded = (args.dist / (name + '.sha256')).read_text()
            expected = f'{hashlib.sha256((args.dist / name).read_bytes()).hexdigest()}  {name}\n'
            if recorded != expected:
                raise ValueError(f'Build artifact checksum mismatch: {name}')
        package_source(args.tag, args.dist, Path('.'))
        (args.dist / 'SHA256SUMS').write_text(''.join(
            f'{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n'
            for path in sorted(args.dist.glob('*.tar.gz'))))
    verify_bundle(args.tag, args.dist)
    print(f'Release bundle verified: {args.tag}')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, tarfile.TarError) as error:
        raise SystemExit(f'release-bundle: {error}') from error
