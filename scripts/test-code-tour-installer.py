#!/usr/bin/env python3
"""Check fresh installation, upgrade, idempotence, and refusal without changing the user's plugin."""
from pathlib import Path
import hashlib
import shutil
import subprocess
import sys
import tempfile

repo = Path(__file__).resolve().parents[1]
source = Path(sys.argv[1]).resolve()
patches = [repo / 'integrations/neovim' / name for name in ('code-tour-standalone.patch', 'code-tour-lazy-agent.patch')]
with tempfile.TemporaryDirectory(prefix='tanki-tour-installer-test-', dir='/tmp') as temporary:
    plugin = Path(temporary) / 'plugin'
    def copy_source():
        for name in ('bin/code-tour', 'lua/code-tour/tour.lua', 'lua/code-tour/saved.lua', 'lua/code-tour/lazy.lua'):
            target = plugin / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / name, target)
    def digest():
        return {str(p.relative_to(plugin)): hashlib.sha256(p.read_bytes()).hexdigest() for p in plugin.rglob('*') if p.is_file()}
    def install(*flags, expected=0):
        result = subprocess.run([sys.executable, str(repo / 'scripts/install-code-tour-bridge.py'), '--plugin', str(plugin), *flags], capture_output=True, text=True)
        assert result.returncode == expected, result.stdout + result.stderr
        return result.stdout
    def reverse(patch):
        subprocess.run(['patch', '--batch', '--reverse', '--force', '-p1', '-d', str(plugin), '-i', str(patch)], check=True, capture_output=True)
    copy_source()
    complete = digest()
    assert 'already installed' in install('--check')
    assert 'already installed' in install()
    reverse(patches[1])
    assert 'code-tour-lazy-agent.patch' in install('--check')
    install()
    assert digest() == complete
    reverse(patches[1]); reverse(patches[0])
    before = digest()
    assert 'code-tour-standalone.patch' in install('--check')
    assert digest() == before, 'Check must not modify installation'
    install()
    assert digest() == complete
    target = plugin / 'bin/code-tour'
    target.write_text(target.read_text().replace("opts['standalone'] = args.standalone or bool(args.lazy_agent)", 'incompatible modification'))
    before = digest()
    install(expected=1)
    assert digest() == before, 'Rejected install must preserve every input file'
print('Installer fresh/upgrade/idempotence/refusal checks passed')
