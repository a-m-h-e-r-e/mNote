-- pins: a short list of notes to jump between, like Neovim's harpoon.
--
--   :Pin  (space a)    pins this note
--   :Pins              lists the pins; pick one to open it
--   :Unpin             takes this note off the list
--   :PinLink           puts [[this note]] on the clipboard
--   space 1 … space 4  open pin 1 to 4
--
-- The list is kept in ~/.config/mnote/pins.txt, one note per line, so it
-- can be edited by hand (and outside the plugin's folder, so saving it
-- doesn't change the plugin's code).

local file = (os.getenv("HOME") or ".") .. "/.config/mnote/pins.txt"

local function load()
  local pins = {}
  local f = io.open(file, "r")
  if f then
    for line in f:lines() do
      if line ~= "" then
        table.insert(pins, line)
      end
    end
    f:close()
  end
  return pins
end

local function save(pins)
  local f = io.open(file, "w")
  if not f then
    mnote.notify("pins: can't write " .. file)
    return
  end
  f:write(table.concat(pins, "\n"), "\n")
  f:close()
end

local function find(pins, note)
  for i, pin in ipairs(pins) do
    if pin == note then
      return i
    end
  end
end

mnote.command.create("Pin", function()
  local note = mnote.note()
  if not note then
    mnote.notify("Only notes can be pinned")
    return
  end
  local pins = load()
  local at = find(pins, note)
  if at then
    mnote.notify(note .. " is pin " .. at)
    return
  end
  table.insert(pins, note)
  save(pins)
  mnote.notify("Pinned " .. note .. " as " .. #pins)
end, { desc = "Pins this note" })

mnote.command.create("Unpin", function()
  local pins = load()
  local at = find(pins, mnote.note())
  if not at then
    mnote.notify("This note isn't pinned")
    return
  end
  table.remove(pins, at)
  save(pins)
  mnote.notify("Unpinned; " .. #pins .. " left")
end, { desc = "Unpins this note" })

mnote.command.create("Pins", function()
  local pins = load()
  if #pins == 0 then
    mnote.notify("No pins yet: :Pin (space a) adds this note")
    return
  end
  local items = {}
  for i, pin in ipairs(pins) do
    table.insert(items, { text = pin, detail = "space " .. i })
  end
  mnote.picker.open({
    title = "Pins",
    items = items,
    on_select = function(note)
      mnote.open(note)
    end,
  })
end, { desc = "Lists the pinned notes" })

mnote.command.create("PinLink", function()
  local note = mnote.note()
  if not note then
    return
  end
  local name = note:gsub("%.md$", "")
  mnote.clipboard.set("[[" .. name .. "]]")
  mnote.notify("Copied [[" .. name .. "]]")
end, { desc = "Copies a link to this note" })

mnote.keymap.set("n", "<leader>a", ":Pin", { desc = "Pins this note" })
for i = 1, 4 do
  mnote.keymap.set("n", "<leader>" .. i, function()
    local note = load()[i]
    if note then
      mnote.open(note)
    else
      mnote.notify("No pin " .. i .. " yet")
    end
  end, { desc = "Opens pin " .. i })
end
