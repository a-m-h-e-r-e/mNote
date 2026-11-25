# Example plugins

- `tasks`: open tasks get a gutter sign and a count on the first line;
  `:Tasks` (space T) lists them in a picker; ```progress blocks draw as bars.
- `wordgoal`: the note's word count against a goal, at the right of the
  first line; `:WordGoal` says how far there is to go.
- `pins`: a few notes to jump between, like Neovim's harpoon. `:Pin`
  (space a) pins the note, `:Pins` lists them, space 1 to 4 open one,
  `:PinLink` copies a `[[link]]` to the note.

To install one, copy its folder into `~/.config/mnote/plugins/` and turn it
on in config.toml (`space ,`):

```toml
plugins.tasks.enabled = true
```

The first time it would run, mNote asks for your OK. The API is in
[docs/plugins.md](../../docs/plugins.md).
