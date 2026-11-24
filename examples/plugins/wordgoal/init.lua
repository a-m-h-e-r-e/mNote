-- wordgoal: an example mNote plugin. Shows "312 / 500 words" at the right of
-- the first line, in the "done" colour once the goal is reached.
--
-- Install: copy this folder to ~/.config/mnote/plugins/wordgoal, then in
-- config.toml:
--
--   [plugins.wordgoal]
--   enabled = true
--   goal = 750          # optional; 500 without it

local function goal()
  local n = tonumber(mnote.config("wordgoal").goal)
  return (n and n > 0) and n or 500
end

local function count()
  local _, words = mnote.buf.get_text():gsub("%S+", "")
  return words
end

local function show()
  mnote.undecorate("wordgoal")
  local words, target = count(), goal()
  mnote.decorate("wordgoal", {
    line = 0,
    text = words .. " / " .. target .. " words",
    color = words >= target and "insert" or "ink_3",
    right = true,
  })
end

mnote.on("NoteOpened", show)
mnote.on("TextChanged", show)

mnote.command.create("WordGoal", function()
  local words, target = count(), goal()
  if words >= target then
    mnote.notify("Goal reached: " .. words .. " words")
  else
    mnote.notify((target - words) .. " words to go")
  end
end, { desc = "How far to today's word goal" })
