#!/usr/bin/env python3
"""Apply the reviewed tanki patches to an installed code-tour.nvim checkout."""
import argparse
from pathlib import Path
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--plugin', type=Path, required=True)
parser.add_argument('--check', action='store_true', help='Check applicability without writing to the plugin')
args = parser.parse_args()
plugin = args.plugin.resolve(strict=True)
patch_dir = Path(__file__).resolve().parents[1] / 'integrations/neovim'
patches = [patch_dir / name for name in ('code-tour-standalone.patch', 'code-tour-lazy-agent.patch')]
pending = []
def command(root, patch, *flags):
    return ['patch', '--batch', '-p1', '-i', str(patch), '-d', str(root), *flags]
# Validate the full chain on copies of only the affected source files before
# touching the installation. Existing applied patches are detected by reversal.
with tempfile.TemporaryDirectory(prefix='tanki-tour-install-', dir='/tmp') as temporary:
    stage = Path(temporary)
    for relative in ('bin/code-tour', 'lua/code-tour/tour.lua', 'lua/code-tour/saved.lua', 'lua/code-tour/lazy.lua'):
        source = plugin / relative
        if source.exists():
            target = stage / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    # Later patches can modify earlier hunks. Unwind applied patches in reverse
    # order on the copies, then validate the complete forward chain.
    applied = set()
    for patch in reversed(patches):
        reverse = subprocess.run(command(stage, patch, '--reverse', '--force', '--dry-run'), capture_output=True, text=True)
        if reverse.returncode == 0:
            subprocess.run(command(stage, patch, '--reverse', '--force'), check=True, capture_output=True, text=True)
            applied.add(patch)
    for patch in patches:
        check = subprocess.run(command(stage, patch, '--forward', '--dry-run'), capture_output=True, text=True)
        if check.returncode:
            raise SystemExit('Plugin differs from the supported version; nothing was written.\n' + check.stdout + check.stderr)
        subprocess.run(command(stage, patch, '--forward'), check=True, capture_output=True, text=True)
        if patch not in applied:
            pending.append(patch)
if not pending:
    print('Tanki bridge patches are already installed')
elif args.check:
    print('Applicable patches: ' + ', '.join(p.name for p in pending))
else:
    for patch in pending:
        subprocess.run(command(plugin, patch, '--forward'), check=True)
    print('Tanki bridge installed. Restart Neovim before using tanki tours.')
