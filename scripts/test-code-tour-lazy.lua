-- Synthetic Herdr/Claude lifecycle; never starts a real agent or contacts a model.
vim.opt.rtp:prepend(assert(vim.env.CODE_TOUR_TEST_PLUGIN))
vim.o.lines, vim.o.columns = 45, 120
require('code-tour.instance').register = function() end
local root = vim.fn.tempname()
vim.fn.mkdir(root .. '/.git', 'p')
vim.fn.writefile({'local answer = 42', 'return answer'}, root .. '/source.lua')
local tour = require('tour')
tour.setup({smooth_scroll=false})
local splits, starts, sent, block = 0, 0, {}, false
local agents = {}
local function reply(callback, result, error)
  callback({code=error and 1 or 0, stdout=vim.json.encode(error and {error={message=error}} or {result=result}), stderr=''})
end
vim.system = function(cmd, _, callback)
  assert(cmd[1] == (vim.env.HERDR_BIN_PATH or 'herdr') and cmd[2] == '--session')
  if cmd[4] == 'pane' and cmd[5] == 'split' then
    splits = splits + 1
    reply(callback, {pane={pane_id='agent-' .. splits}})
  elseif cmd[4] == 'agent' and cmd[5] == 'list' then
    local all = {}; for _, agent in pairs(agents) do all[#all+1] = agent end
    reply(callback, {agents=all})
  elseif cmd[4] == 'agent' and cmd[5] == 'start' then
    starts = starts + 1
    local pane
    for i, v in ipairs(cmd) do if v == '--pane' then pane = cmd[i+1] end end
    agents[pane] = {pane_id=pane, agent='claude', agent_status=block and 'blocked' or 'idle', tab_id='reader-tab', agent_session={value='session-' .. pane}}
    reply(callback, {}, block and 'startup needs approval' or nil)
  elseif cmd[4] == 'pane' and cmd[5] == 'process-info' then
    reply(callback, {process_info={foreground_processes={{pid=42}}}})
  else error('Unexpected command: ' .. vim.inspect(cmd)) end
end
require('code-tour.conversation').send = function(origin, prompt, _, callback)
  assert(origin and origin.agent == 'claude' and origin.process_ids[1] == 42)
  sent[#sent+1] = prompt
  callback(true)
end
local function open(reader)
  tour.show({{file='source.lua',start=1,['end']=2,note='Describe this function'}}, {root=root})
  local saved = tour.save_saved({title='Synthetic lazy review'})
  return tour.open_saved(saved.id, {standalone=true, lazy_agent={kind='claude',session='synthetic',pane_id=reader,root=root}})
end
local function wait_for(predicate)
  assert(vim.wait(2000, predicate, 5), 'Timed out in synthetic lazy lifecycle')
end
local function run()
  open('reader-1')
  assert(splits == 0 and starts == 0 and #sent == 0)
  tour.ask(nil)
  assert(splits == 0 and starts == 0, 'Opening a composer must not start Claude')
  require('code-tour.prompt').clear()
  tour.ask('What is the return value?')
  wait_for(function() return #sent == 1 end)
  assert(splits == 1 and starts == 1)
  assert(sent[1]:find('Captured current source',1,true) and sent[1]:find('local answer = 42',1,true))
  assert(tour.state().origin.pane_id == 'agent-1')
  tour.ask('Why?')
  assert(#sent == 2 and splits == 1 and starts == 1, 'A linked Claude is reused')
  open('reader-1')
  tour.ask('A later saved tour in this reader')
  wait_for(function() return #sent == 3 end)
  assert(splits == 1 and starts == 1, 'The reader reuses its owned Claude pane across tours')
  open('reader-2')
  tour.ask('Cancelled by replacing tour')
  tour.show({{file='source.lua',start=1,['end']=2,note='Replacement'}}, {root=root})
  wait_for(function() return starts == 2 end)
  vim.wait(30, function() return false end, 5)
  assert(#sent == 3 and tour.state().origin == nil, 'Old tour must not submit a late prompt')
  block = true
  open('reader-3')
  tour.ask('Wait for startup')
  wait_for(function() return starts == 3 end)
  vim.wait(30, function() return false end, 5)
  tour.ask('Still waiting')
  vim.wait(30, function() return false end, 5)
  assert(splits == 3 and starts == 3 and #sent == 3, 'Blocked startup must not duplicate panes or submit input')
  agents['agent-3'].agent_status = 'idle'
  tour.ask('Startup completed by the user')
  wait_for(function() return #sent == 4 end)
  assert(splits == 3 and starts == 3)
end
local ok, error = pcall(run)
vim.fn.delete(root, 'rf')
if not ok then io.stderr:write(tostring(error)); vim.cmd('cquit 1') end
vim.cmd('qa!')
