# Example plugins

- `tasks`: open tasks get a gutter sign and a count on the first line;
  `:Tasks` (space T) lists them in a picker; ```progress blocks draw as bars.
- `wordgoal`: the note's word count against a goal, at the right of the
  first line; `:WordGoal` says how far there is to go.

To install one, copy its folder into `~/.config/mnote/plugins/` and turn it
on in config.toml (`space ,`):

```toml
plugins.tasks.enabled = true
```

The first time it would run, mNote asks for your OK. The API is in
[docs/plugins.md](../../docs/plugins.md).
