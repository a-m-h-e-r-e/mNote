-- tasks: an example mNote plugin. Install: copy this folder to
-- ~/.config/mnote/plugins/tasks and add `plugins.tasks.enabled = true` to
-- config.toml.
--
-- * Open tasks (`- [ ]`) get a sign in the gutter, and the first line says
--   how many there are, as the note opens and as you type.
-- * :Tasks (or space T) lists them in a picker; Enter goes to one.
-- * A ```progress block draws its lines as bars:
--
--     ```progress
--     Writing: 3/10
--     Reading: 7/7
--     ```

-- Marks this note's open tasks and returns them.
local function scan()
  mnote.undecorate("tasks")
  local open = {}
  for i, line in ipairs(mnote.buf.get_lines(0, -1)) do
    if line:match("^%s*[-*] %[ %]") then
      local text = line:gsub("^%s*[-*] %[ %]%s*", "")
      table.insert(open, { line = i - 1, text = text })
      mnote.decorate("tasks", { line = i - 1, sign = "○", color = "insert" })
    end
  end
  if #open > 0 then
    local label = #open == 1 and "1 open task" or (#open .. " open tasks")
    mnote.decorate("tasks", { line = 0, text = label, right = true })
  end
  return open
end

mnote.on("NoteOpened", scan)
mnote.on("TextChanged", scan)

mnote.command.create("Tasks", function()
  local open = scan()
  if #open == 0 then
    mnote.notify("No open tasks here")
    return
  end
  local items = {}
  for _, task in ipairs(open) do
    table.insert(items, { text = task.text, detail = "line " .. (task.line + 1) })
  end
  mnote.picker.open({
    title = "Open tasks",
    items = items,
    on_select = function(_, i)
      mnote.buf.set_cursor(open[i].line, 0)
    end,
  })
end, { desc = "List this note's open tasks" })

mnote.keymap.set("n", "<leader>T", "Tasks")

mnote.blocks.register("progress", {
  render = function(lines)
    local rows = {}
    for _, line in ipairs(lines) do
      local label, done, total = line:match("^(.-):%s*(%d+)%s*/%s*(%d+)")
      if label then
        done, total = tonumber(done), tonumber(total)
        local width = 20
        local filled = total > 0 and math.floor(width * done / total) or 0
        table.insert(rows, {
          text = string.format("%-10s %s%s %3d%%", label,
            string.rep("█", filled), string.rep("░", width - filled),
            total > 0 and math.floor(100 * done / total) or 0),
          color = done >= total and "insert" or nil,
        })
      end
    end
    return rows
  end,
})
