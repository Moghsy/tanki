#!/usr/bin/env python3
"""Exercise the companion patch using only synthetic tours/XDG profiles.
Usage: python3 scripts/test-code-tour-bridge.py /path/to/code-tour.nvim
"""
import argparse
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('plugin', type=Path)
parser.add_argument('--tanki', type=Path, help='Also exercise the real binary and Neovim RPC with temporary sockets')
args = parser.parse_args()
plugin = args.plugin.resolve()
loader = importlib.machinery.SourceFileLoader('tanki_tour_test', str(plugin / 'bin/code-tour'))
spec = importlib.util.spec_from_loader(loader.name, loader)
cli = importlib.util.module_from_spec(spec)
loader.exec_module(cli)
with tempfile.TemporaryDirectory(prefix='tanki-tour-bridge-', dir='/tmp') as temporary:
    root = Path(temporary)
    env = dict(os.environ, HOME=str(root / 'home'), XDG_DATA_HOME=str(root / 'data'),
               XDG_STATE_HOME=str(root / 'state'), XDG_CONFIG_HOME=str(root / 'config'),
               XDG_CACHE_HOME=str(root / 'cache'), NVIM_APPNAME='tanki-test')
    with patch.dict(os.environ, env, clear=True), patch.object(cli, 'instances', side_effect=AssertionError('editor discovery')):
        assert cli.main(['capabilities']) == 0
    lua = r'''
vim.opt.rtp:prepend(PLUGIN)
vim.o.lines, vim.o.columns = 45, 100
require('code-tour.instance').register = function() end
local tour = require('tour')
local sends = 0
require('code-tour.conversation').send = function() sends = sends + 1; error('Unexpected generation') end
local source = ROOT .. '/repo'
vim.fn.mkdir(source .. '/.git', 'p')
vim.fn.writefile({'local x = 1', 'return x'}, source .. '/test.lua')
tour.setup({smooth_scroll=false})
tour.show({{file='test.lua', start=1, ['end']=2, note='An authored explanation'}}, {root=source, origin={agent='old'}})
local saved = tour.save_saved({title='Synthetic tanki tour'})
local opened = tour.open_saved(saved.id, {standalone=true})
assert(tour.state().origin == nil and tour.state().standalone)
assert(opened.opened == 1 and sends == 0)
assert(opened.counts.ready == 1, vim.inspect(opened))
tour.ask('test'); tour.explain(false); tour.explain_definition(true)
assert(sends == 0)
vim.fn.writefile({'local changed = 42', 'return changed'}, source .. '/test.lua')
vim.cmd('edit! ' .. vim.fn.fnameescape(source .. '/test.lua'))
opened = tour.open_saved(saved.id, {standalone=true})
assert(opened.counts.unavailable == 1, vim.inspect(opened))
assert(sends == 0 and tour.state().saved.active == nil)
local token = tour.state().id
local ok = pcall(tour.open_saved, saved.id, {standalone=true, repositories={['bad']='relative'}})
assert(not ok and tour.state().id == token)
vim.fn.writefile({saved.id}, ROOT .. '/id')
-- The optional shortcut uses the live owner API, never a second CLI process.
vim.opt.rtp:append(INTEGRATION)
local inputs = { 'Question <x>', 'Answer & details', 'Default' }
vim.ui.input = function(_, callback) callback(table.remove(inputs, 1)) end
local called = false
vim.system = function(command, opts, callback)
  called = true
  assert(command[1] == 'curl' and command[#command] == 'http://127.0.0.1:8765')
  local body = vim.json.decode(opts.stdin)
  assert(body.key == 'secret' and body.action == 'addNote')
  assert(body.params.note.fields.CodeTour == 'code-tour://saved/' .. saved.id)
  assert(body.params.note.fields.Prompt == 'Question &lt;x&gt;')
  callback({code=0,stdout='{"result":123,"error":null}'})
end
require('tanki').setup({api_key='secret'})
vim.cmd('TankiAddTourCard')
vim.wait(10, function() return false end, 1)
assert(called and #inputs == 0)
'''.replace('PLUGIN', json.dumps(str(plugin))).replace('ROOT', json.dumps(str(root))).replace('INTEGRATION', json.dumps(str(Path(__file__).resolve().parents[1] / 'integrations/neovim')))
    script = root / 'test.lua'
    script.write_text("local ok, err = pcall(function()\n" + lua + "\nend)\nif not ok then io.stderr:write(tostring(err)); vim.cmd('cquit 1') end\nvim.cmd('qa!')\n")
    result = subprocess.run(['nvim', '--clean', '--headless', '-n', '-i', 'NONE', '-l', str(script)], env=env,
                            capture_output=True, text=True, timeout=20)
    if result.returncode:
        raise SystemExit(result.stdout + result.stderr)
    identity = (root / 'id').read_text().strip()
    with patch.dict(os.environ, env, clear=True), patch.object(cli, 'instances', side_effect=AssertionError('editor discovery')):
        assert cli.main(['saved', 'inspect', identity]) == 0
    editor = dict(address='/synthetic/socket', instance_id='instance', cwd=str(root))
    delivered = []
    with patch.dict(os.environ, dict(env, HERDR_SESSION='normal-tanki-pane'), clear=True), \
            patch.object(cli, 'instances', return_value=[editor]), \
            patch.object(cli, 'choose', return_value=editor), \
            patch.object(cli, 'remember'), \
            patch.object(cli, 'origin', side_effect=AssertionError('origin discovery')), \
            patch.object(cli, 'focus', side_effect=RuntimeError('Cannot focus')), \
            patch.object(cli, 'deliver', side_effect=lambda target, method, arguments, **kw: delivered.append(arguments) or dict(id=identity, opened=1)):
        assert cli.main(['saved', 'open', identity, '--pane', editor['address'], '--standalone']) == 0
        assert delivered[0][1]['standalone'] and 'origin' not in delivered[0][1]
    launches = []
    def fake_herdr(*parts, **kwargs):
        launches.append(parts)
        if parts[:2] == ('tab', 'create'):
            return dict(root_pane=dict(pane_id='new-reader'))
        return {}
    with patch.dict(os.environ, dict(env, HERDR_SESSION='synthetic'), clear=True), \
            patch.object(cli, 'herdr', side_effect=fake_herdr), \
            patch.object(cli, 'inspect', return_value=dict(address='/tmp/synthetic-editor')):
        cli.launch(root / 'repo')
    assert '--no-focus' in launches[0] and '--env' in launches[0]
    assert 'NVIM_APPNAME=tanki-test' in launches[0]
    assert launches[1][:3] == ('pane', 'run', 'new-reader')
    lazy_test = Path(__file__).with_name('test-code-tour-lazy.lua')
    lazy_result = subprocess.run(['nvim', '--clean', '--headless', '-n', '-i', 'NONE', '-l', str(lazy_test)],
                                 env=dict(env, CODE_TOUR_TEST_PLUGIN=str(plugin)), capture_output=True, text=True, timeout=20)
    assert lazy_result.returncode == 0, lazy_result.stdout + lazy_result.stderr
    print('Lazy Claude lifecycle checks passed (mock Herdr, no model requests)')
    if args.tanki:
        env = {k: v for k, v in env.items() if not k.startswith(('HERDR_', 'CODEX_', 'CLAUDE_'))}
        address = str(root / 'nvim.sock')
        setup = 'lua vim.opt.rtp:prepend(' + json.dumps(str(plugin)) + '); require("tour").setup({smooth_scroll=false})'
        editor = subprocess.Popen(['nvim', '--clean', '--headless', '-n', '-i', 'NONE', '--listen', address, '-c', setup],
                                  cwd=root / 'repo', env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        def command(argv, input=None):
            result = subprocess.run(argv, input=input, env=env, cwd=root / 'repo', capture_output=True, text=True, timeout=30)
            assert result.returncode == 0, result.stdout + result.stderr
            return json.loads(result.stdout)
        try:
            deadline = time.monotonic() + 10
            while not Path(address).exists():
                assert editor.poll() is None and time.monotonic() < deadline, 'Test editor failed to start'
                time.sleep(0.02)
            cli_argv = [sys.executable, str(plugin / 'bin/code-tour')]
            command(cli_argv + ['--pane', address], json.dumps([dict(file='test.lua', start=1, end=2, note='Synthetic saved tour')]))
            saved = command(cli_argv + ['saved', 'save', '--title', 'End-to-end tanki tour', '--pane', address])
            config = root / 'settings.toml'
            config.write_text('[code_tour]\nenabled = true\nnvim_appname = "tanki-test"\ncommand = ' + json.dumps(cli_argv) + '\n')
            base = root / 'anki'
            base.mkdir()
            tanki_argv = [str(args.tanki.resolve()), '--base', str(base), '--profile', 'synthetic', '--config', str(config), '--json']
            command(tanki_argv + ['profiles', 'create', 'synthetic'])
            command(tanki_argv + ['tour', 'install-note-type'])
            created = command(tanki_argv + ['tour', 'add', '--tour', saved['id'], '--prompt', 'How does it work?', '--answer', 'Read the saved tour'])
            assert created['note_id'] > 0
            # Simulate an ordinary Herdr tanki pane with no authoring agent.
            env['HERDR_SESSION'] = 'synthetic-no-agent'
            opened = command(tanki_argv + ['tour', 'open', saved['id'], '--pane', address])
            assert opened['id'] == saved['id'] and opened['opened'] == 1
            live = cli.rpc(address, 'local s=require("tour").state(); return {standalone=s.standalone, origin=s.origin, active=s.saved.active~=nil}')
            assert live['standalone'] and not live.get('origin') and not live['active']
            assert command(tanki_argv + ['tour', 'check'])[0]['result']['status'] == 'available'
            print('Real tanki -> CLI -> Neovim RPC checks passed')
        finally:
            editor.terminate()
            editor.wait(timeout=5)
            editor.stderr.close()
print('Standalone bridge checks passed')
