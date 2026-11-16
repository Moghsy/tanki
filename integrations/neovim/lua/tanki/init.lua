-- Optional creation shortcut; tanki remains the sole collection owner.
local M = {}
local options = { endpoint = "http://127.0.0.1:8765", deck = "Default", api_key = nil }
function M.add_tour_card()
  local ok, tour = pcall(require, "tour")
  local saved = ok and tour.state().saved
  local id = saved and saved.record and saved.record.id
  if type(id) ~= "string" or #id ~= 32 or not id:match("^[0-9a-f]+$") then
    vim.notify("Open an explicitly saved code-tour first", vim.log.levels.WARN)
    return
  end
  vim.ui.input({ prompt = "Card prompt: " }, function(prompt)
    if not prompt or not prompt:find("%S") then return end
    vim.ui.input({ prompt = "Card answer: " }, function(answer)
      if answer == nil then return end
      vim.ui.input({ prompt = "Tanki deck: ", default = options.deck }, function(deck)
        if not deck or not deck:find("%S") then return end
        local function html(text)
          return text:gsub("&", "&amp;"):gsub("<", "&lt;"):gsub(">", "&gt;"):gsub("\n", "<br>")
        end
        local request = { action = "addNote", version = 6, key = options.api_key,
          params = { note = { deckName = deck, modelName = "Tanki Code Tour",
            fields = { Prompt = html(prompt), Answer = html(answer), CodeTour = "code-tour://saved/" .. id },
            options = { allowDuplicate = false }, tags = { "tanki-code-tour" } } } }
        local spawned, spawn_error = pcall(vim.system, { "curl", "--silent", "--show-error", "--max-time", "5", "--header", "Content-Type: application/json",
          "--data-binary", "@-", options.endpoint }, { stdin = vim.json.encode(request), text = true },
          vim.schedule_wrap(function(result)
            if result.code ~= 0 then
              vim.notify("Tanki API unavailable: " .. (result.stderr or "curl failed"), vim.log.levels.ERROR)
              return
            end
            local parsed, response = pcall(vim.json.decode, result.stdout)
            if not parsed or type(response) ~= "table" or response.error ~= vim.NIL and response.error ~= nil or type(response.result) ~= "number" then
              local message = parsed and type(response) == "table" and vim.inspect(response.error) or "invalid JSON"
              vim.notify("Tanki could not add card: " .. message, vim.log.levels.ERROR)
              return
            end
            vim.notify("Added tanki note " .. tostring(response.result), vim.log.levels.INFO)
          end))
        if not spawned then vim.notify("Could not start curl: " .. tostring(spawn_error), vim.log.levels.ERROR) end
      end)
    end)
  end)
end
function M.setup(opts)
  options = vim.tbl_extend("force", options, opts or {})
  vim.api.nvim_create_user_command("TankiAddTourCard", M.add_tour_card, { desc = "Add a card for the current saved code-tour", force = true })
end
return M
