"""Offline release gates; fixture bytes are not platform execution evidence."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
import os

ROOT = Path(__file__).resolve().parents[2]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / f'{name}.py')
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.gate = module('release_gate')
        self.sha = 'a' * 40
        self.run = dict(head_sha=self.sha, path='.github/workflows/ci.yml',
                        event='workflow_dispatch', status='completed', conclusion='success',
                        repository={'full_name': 'mm9942/Harwness'})
        self.jobs = [dict(name=name, status='completed', conclusion='success',
                          steps=[dict(name=s, status='completed', conclusion='success')
                                 for s in steps])
                     for name, steps in self.gate.REQUIRED_JOBS.items()]

    def test_complete_exact_commit_evidence_passes(self):
        self.gate.validate_ci(self.run, self.jobs, self.sha, 'mm9942/Harwness')

    def test_required_steps_match_real_ci_step_names(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        for steps in self.gate.REQUIRED_JOBS.values():
            for step in steps:
                self.assertIn(f'name: {step}\n', workflow)

    def test_metadata_rejects_wrong_tag_and_upcoming_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'crate').mkdir()
            (root / 'Cargo.toml').write_text('[workspace]\nmembers=["crate"]\n[workspace.package]\nversion="0.10.0"\n')
            (root / 'crate/Cargo.toml').write_text('[package]\nname="crate"\nversion.workspace=true\n')
            (root / 'Cargo.lock').write_text('[[package]]\nname="crate"\nversion="0.10.0"\n')
            (root / 'CHANGELOG.md').write_text('## [0.10.0] — Upcoming\n')
            self.gate.validate_metadata('v0.10.0', root)
            with self.assertRaises(ValueError):
                self.gate.validate_metadata('v0.9.1', root)
            with self.assertRaises(ValueError):
                self.gate.validate_metadata('v0.10.0', root, publishing=True)
            (root / 'CHANGELOG.md').write_text('## [0.10.0] — 2026-10-30\n')
            self.gate.validate_metadata('v0.10.0', root, publishing=True)

    def test_absent_wrong_or_skipped_evidence_is_rejected(self):
        for field, value in [('head_sha', 'b' * 40), ('event', 'pull_request'),
                             ('conclusion', 'failure'), ('status', 'queued'),
                             ('path', '.github/workflows/release.yml')]:
            with self.subTest(field=field):
                run = dict(self.run, **{field: value})
                with self.assertRaises(ValueError):
                    self.gate.validate_ci(run, self.jobs, self.sha, 'mm9942/Harwness')
        for jobs in [[], self.jobs[:-1], self.jobs + [self.jobs[0]]]:
            with self.assertRaises(ValueError):
                self.gate.validate_ci(self.run, jobs, self.sha, 'mm9942/Harwness')
        for mode in ['skipped', 'no-steps']:
            jobs = copy.deepcopy(self.jobs)
            if mode == 'skipped':
                jobs[1]['steps'][0]['conclusion'] = 'skipped'
            else:
                jobs[1]['steps'] = []
            with self.assertRaises(ValueError):
                self.gate.validate_ci(self.run, jobs, self.sha, 'mm9942/Harwness')


class BundleTests(unittest.TestCase):
    TAG = 'v0.10.0'

    def setUp(self):
        self.bundle = module('release_bundle')
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.dist = Path(self.temp.name)
        for target in self.bundle.LINUX_TARGETS:
            root = f'harw-{self.TAG}-{target}'
            self.archive(root + '.tar.gz', root,
                         ['harw', 'killer', 'harw-job-exec', 'harw-agent-runner',
                          'README.md', 'LICENSE-MIT', 'LICENSE-APACHE'])
        self.archive('harwness-0.10.0-source.tar.gz', 'harwness-0.10.0',
                     ['Cargo.toml', 'Cargo.lock', 'Makefile', 'scripts/install.sh'])
        self.sums()

    def archive(self, name, root, files):
        with tarfile.open(self.dist / name, 'w:gz', format=tarfile.USTAR_FORMAT) as tar:
            for file in files:
                info = tarfile.TarInfo(f'{root}/{file}')
                info.mode = 0o755
                info.size = 3
                tar.addfile(info, io.BytesIO(b'abc'))

    def sums(self):
        (self.dist / 'SHA256SUMS').write_text(''.join(
            f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n'
            for p in sorted(self.dist.glob('*.tar.gz'))))

    def test_complete_bundle_passes(self):
        self.bundle.verify_bundle(self.TAG, self.dist)

    def test_missing_linux_or_source_and_corruption_are_rejected(self):
        for path in list(self.dist.glob('*.tar.gz')):
            data = path.read_bytes()
            path.unlink()
            self.sums()
            with self.assertRaises(ValueError):
                self.bundle.verify_bundle(self.TAG, self.dist)
            path.write_bytes(data)
            self.sums()
            path.write_bytes(data + b'corruption')
            with self.assertRaises(ValueError):
                self.bundle.verify_bundle(self.TAG, self.dist)
            path.write_bytes(data)
            self.sums()

    def test_valid_checksum_cannot_hide_a_missing_trampoline(self):
        target = self.bundle.LINUX_TARGETS[0]
        root = f'harw-{self.TAG}-{target}'
        self.archive(root + '.tar.gz', root, ['harw', 'harw-agent-runner', 'killer'])
        self.sums()
        with self.assertRaises(ValueError):
            self.bundle.verify_bundle(self.TAG, self.dist)

    def test_source_is_ustar_and_has_no_pax_headers(self):
        self.bundle.package_source(self.TAG, self.dist, ROOT)
        with tarfile.open(self.dist / 'harwness-0.10.0-source.tar.gz') as tar:
            self.assertFalse(tar.pax_headers)
            members = tar.getmembers()
            self.assertTrue(all(not m.pax_headers for m in members))
            self.assertIn('harwness-0.10.0/Cargo.toml', [m.name for m in members])

    def test_real_packager_output_has_installer_layout(self):
        target = self.bundle.LINUX_TARGETS[0]
        repo = self.dist / 'repo'
        repo.mkdir()
        for name in self.bundle.DOCUMENTS:
            (repo / name).write_text(name)
        for profile, names in [('release', ['harw', 'killer', 'harw-job-exec']),
                               ('release-runner', ['harw-agent-runner'])]:
            directory = repo / 'target' / target / profile
            directory.mkdir(parents=True)
            for name in names:
                path = directory / name
                path.write_text('#!/bin/sh\nexit 0\n')
                path.chmod(0o755)
        subprocess.run(['bash', str(ROOT / 'scripts/package-release.sh'), self.TAG,
                        target, str(self.dist)], cwd=repo, check=True, capture_output=True)
        self.sums()
        self.bundle.verify_bundle(self.TAG, self.dist)


class MirrorTests(BundleTests):
    # Reuse the complete bundle fixture, exercise cloud boundaries offline.
    def mirror(self, fail=False):
        mocks, bucket = self.dist / 'mocks', self.dist / 'bucket'
        mocks.mkdir()
        bucket.mkdir()
        aws = mocks / 'aws'
        aws.write_text('''#!/usr/bin/env python3
import os, pathlib, shutil, sys
args=sys.argv[1:]; args=args[args.index('--only-show-errors')+1:]
source,destination=args[:2]
log=pathlib.Path(os.environ['TEST_LOG'])
with log.open('a') as f:f.write(source+' '+destination+'\\n')
def path(s):
 return pathlib.Path(os.environ['TEST_BUCKET'])/s.removeprefix('s3://harw/harw/') if s.startswith('s3://') else pathlib.Path(s)
src,dst=path(source),path(destination)
if os.environ.get('TEST_FAIL')=='1' and source.startswith('s3://'):sys.exit(1)
dst.parent.mkdir(parents=True,exist_ok=True); shutil.copyfile(src,dst)
''')
        curl = mocks / 'curl'
        curl.write_text('''#!/usr/bin/env python3
import os,pathlib,shutil,sys
args=sys.argv[1:];url=next(x for x in args if x.startswith('https://'))
src=pathlib.Path(os.environ['TEST_BUCKET'])/url.removeprefix('https://get.harw.dev/harw/')
dst=pathlib.Path(args[args.index('-o')+1]);shutil.copyfile(src,dst)
''')
        for path in [aws, curl]:
            path.chmod(0o755)
        env = dict(os.environ, PATH=f'{mocks}:' + os.environ['PATH'],
                   TEST_BUCKET=str(bucket), TEST_LOG=str(self.dist / 'uploads'),
                   TEST_FAIL='1' if fail else '0', R2_ACCOUNT_ID='test',
                   AWS_ACCESS_KEY_ID='test', AWS_SECRET_ACCESS_KEY='test',
                   R2_BUCKET='harw', R2_PREFIX='harw')
        return subprocess.run(['bash', str(ROOT / 'scripts/mirror-release.sh'),
                               self.TAG, str(self.dist)], cwd=ROOT, env=env,
                              text=True, capture_output=True), bucket

    def test_remote_readback_failure_does_not_promote_discovery(self):
        result, bucket = self.mirror(fail=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((bucket / 'latest').exists())
        self.assertFalse((bucket / 'version.json').exists())
        self.assertFalse((bucket / 'install.sh').exists())

    def test_mirror_preserves_github_bytes_and_promotes_last(self):
        result, bucket = self.mirror()
        self.assertEqual(result.returncode, 0, result.stderr)
        for path in self.dist.glob('*.tar.gz'):
            self.assertEqual(path.read_bytes(), (bucket / self.TAG / path.name).read_bytes())
        self.assertEqual((self.dist / 'SHA256SUMS').read_bytes(),
                         (bucket / self.TAG / 'SHA256SUMS').read_bytes())
        manifest = json.loads((bucket / 'version.json').read_text())
        self.assertEqual(set(manifest['targets']), set(self.bundle.LINUX_TARGETS))
        self.assertEqual((bucket / 'latest').read_text(), self.TAG + '\n')


if __name__ == '__main__':
    unittest.main()
