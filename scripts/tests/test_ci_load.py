#!/usr/bin/env python3
"""Release artifact and CI selection contracts, using only the standard library."""

import fnmatch
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
RELEASE = (ROOT / '.github/workflows/build-linux-windows.yml').read_text()
CI = (ROOT / '.github/workflows/ci.yml').read_text()


def job(text, name):
    match = re.search(rf'^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:|\Z)', text, re.M | re.S)
    if match is None:
        raise AssertionError(f'missing job: {name}')
    return match.group(1)


def step_script(block, name):
    step = re.search(rf'^      - name: {re.escape(name)}\n(.*?)(?=^      - |\Z)', block, re.M | re.S)
    if step is None:
        raise AssertionError(f'missing step: {name}')
    return textwrap.dedent(step.group(1).split('        run: |\n', 1)[1])


class CiLoadTests(unittest.TestCase):
    def test_release_keeps_every_target_and_man_dependency(self):
        native = job(RELEASE, 'build-linux-gnu')
        cross = job(RELEASE, 'build-linux')
        windows = job(RELEASE, 'build-windows')
        targets = re.findall(r'^          - ([\w-]+)\s*$', native + cross, re.M)
        self.assertCountEqual(targets, [
            'x86_64-unknown-linux-gnu', 'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-gnu',
        ])
        self.assertIn('needs: [build-linux-gnu]', cross)
        self.assertIn('name: man-pages', native)
        self.assertIn('name: man-pages', cross)
        self.assertIn('--target x86_64-pc-windows-msvc', windows)
        self.assertNotIn('--frozen', windows.split('        run: cargo build', 1)[1].splitlines()[0])
        self.assertNotIn('mlugg/setup-zig', RELEASE)
        self.assertNotIn('  gen-man:', RELEASE)

    def test_compiler_cache_is_explicit_and_excludes_cross_containers(self):
        defaults = re.findall(r'      compiler_cache_pilot:\n(.*?)(?=^      \w|^\S)', RELEASE, re.M | re.S)
        self.assertEqual(len(defaults), 2)
        self.assertTrue(all('default: false' in block for block in defaults))
        for name in ('build-linux-gnu', 'build-windows'):
            block = job(RELEASE, name)
            self.assertIn("inputs.compiler_cache_pilot && 'sccache' || ''", block)
            self.assertIn('sccache --show-stats', block)
        self.assertNotIn('sccache', job(RELEASE, 'build-linux'))

    def test_new_and_old_release_tags_generate_pages_and_propagate_failures(self):
        script = step_script(job(RELEASE, 'build-linux-gnu'), 'Generate man pages from the release binary')
        for mode in ('new', 'old', 'help-error', 'generate-error'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                binary = root / 'target/x86_64-unknown-linux-gnu/release-fast/vtcode'
                binary.parent.mkdir(parents=True)
                binary.write_text('#!/bin/bash\nset -eu\n' + {
                    'new': 'if [[ "$*" == "man --help" ]]; then echo --all-pages; exit; fi\nprintf "%s\\n" "$*" > calls\nmkdir -p man/man1\necho native > man/man1/vtcode.1\n',
                    'old': 'echo "  --allowed-tools <TOOLS>  Global tool allow-list"\necho "  --all  Show all sessions"\n',
                    'help-error': 'exit 7\n',
                    'generate-error': 'if [[ "$*" == "man --help" ]]; then echo --all-pages; exit; fi\nexit 9\n',
                }[mode])
                binary.chmod(0o755)
                prelude = 'cargo() { printf "%s\\n" "$*" > calls; mkdir -p man/man1; echo legacy > man/man1/vtcode.1; }\n'
                result = subprocess.run(['bash', '-eu', '-c', prelude + script], cwd=root, capture_output=True, text=True)
                self.assertEqual(result.returncode, {'new': 0, 'old': 0, 'help-error': 7, 'generate-error': 9}[mode], result.stderr)
                if mode in ('new', 'old'):
                    self.assertEqual((root / 'man/man1/vtcode.1').read_text().strip(), 'native' if mode == 'new' else 'legacy')
                    self.assertEqual((root / 'calls').read_text().strip(), 'man --all-pages --output man/man1' if mode == 'new' else 'run --locked -p xtask -- gen-man --out-dir man/man1')
                else:
                    self.assertFalse((root / 'calls').exists(), 'generation errors must not use legacy fallback')

    def test_linux_archives_include_binary_and_shared_pages(self):
        for name, target in [('build-linux-gnu', 'x86_64-unknown-linux-gnu'), ('build-linux', 'aarch64-unknown-linux-gnu')]:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                binary = root / f'target/{target}/release-fast/vtcode'
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b'expected-release-binary')
                man = root / 'man/man1/vtcode.1'
                man.parent.mkdir(parents=True)
                man.write_text('expected-release-man-page')
                script = step_script(job(RELEASE, name), 'Package').replace('${{ matrix.target }}', target).replace('${{ inputs.tag }}', '1.2.3')
                subprocess.run(['bash', '-eu', '-c', script], cwd=root, check=True, capture_output=True)
                archive = root / f'vtcode-1.2.3-{target}.tar.gz'
                with tarfile.open(archive) as contents:
                    self.assertEqual(contents.extractfile('./vtcode').read(), b'expected-release-binary')
                    self.assertEqual(contents.extractfile('./man/man1/vtcode.1').read(), b'expected-release-man-page')
                self.assertTrue((root / f'vtcode-1.2.3-{target}.sha256').is_file())

    def test_heavy_jobs_are_gated_and_scheduled_runs_remain_full(self):
        selector = job(CI, 'changes')
        for output in ('rust', 'dependencies'):
            self.assertIn(f"github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' || steps.filter.outputs.{output} == 'true'", selector)
        for name in ('formatting', 'lint-clippy', 'test', 'msrv', 'check-windows'):
            block = job(CI, name)
            self.assertIn('needs: changes', block)
            self.assertIn("needs.changes.outputs.rust == 'true'", block)
        for name in ('license-notices', 'unmaintained', 'dependency-audit'):
            self.assertIn("needs.changes.outputs.dependencies == 'true'", job(CI, name))
        # Independently exercise representative paths against the declared filters.
        rust = selector.split('            rust:\n', 1)[1].split('            dependencies:', 1)[0]
        patterns = re.findall(r"- '([^']+)'", rust)
        for path, expected in [('README.md', False), ('docs/guides/example.md', False), ('src/prompts/example.md', True), ('docs/config/CONFIG_FIELD_REFERENCE.md', True), ('Cargo.lock', True), ('scripts/build.sh', True), ('crates/example/src/lib.rs', True)]:
            matched = any(fnmatch.fnmatchcase(path, pattern) or (pattern.startswith('**/') and fnmatch.fnmatchcase(path, pattern[3:])) for pattern in patterns)
            self.assertEqual(matched, expected, path)

    def test_concurrency_groups_cancel_obsolete_runs_without_mixing_events(self):
        for filename in ('ci.yml', 'coverage.yml', 'tool-eval.yml'):
            workflow = (ROOT / '.github/workflows' / filename).read_text()
            group = re.search(r'^  group: (.*)$', workflow, re.M).group(1)
            self.assertIn('github.event_name', group)
            self.assertIn('github.ref', group)
            self.assertNotIn('github.head_ref', group, 'forks can reuse the same branch name')
            self.assertNotIn('github.run_id', group)


if __name__ == '__main__':
    unittest.main()
