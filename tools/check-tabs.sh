#!/bin/sh
# Tabs on every pane end to end (ADR-0014): plays key scripts into a release
# build in
# a throwaway HOME (your own notes and config are never touched) and checks
# the panes, their tabs and the text it reports, and the files it writes.
#
# A state line reads: pane 1/2 | the focused pane's tabs (> the active one) |
# [each other pane's tabs] | parked … | closed … | overlay … | reply … | …
#
#   tools/check-tabs.sh
#
# Needs a display: it opens a window for each script.
set -eu

cd "$(dirname "$0")/.."
cargo build --release -p mnote-app
BIN="$PWD/target/release/mnote"
SANDBOX=$(mktemp -d)
trap '[ -n "${KEEP:-}" ] || rm -rf "$SANDBOX"' EXIT
VAULT="$SANDBOX/home/notes"
mkdir -p "$SANDBOX/home/.config/mnote" "$VAULT"
printf 'vault = "%s"\n' "$VAULT" > "$SANDBOX/home/.config/mnote/config.toml"
failed=0

# run NAME: plays $SANDBOX/NAME.keys on a.md, leaving its state lines in
# $SANDBOX/NAME.out.
run() {
    printf 'alpha\n' > "$VAULT/a.md"
    printf 'beta\n' > "$VAULT/b.md"
    HOME="$SANDBOX/home" XDG_CONFIG_HOME="$SANDBOX/home/.config" \
        MNOTE_SCRIPT="$SANDBOX/$1.keys" "$BIN" "$VAULT/a.md" 2>&1 |
        sed -n 's/^mnote state: //p' > "$SANDBOX/$1.out" || true
}

# expect NAME N TEXT: state line N of NAME has TEXT in it.
expect() {
    line=$(sed -n "$2p" "$SANDBOX/$1.out")
    case "$line" in
    *"$3"*) ;;
    *)
        echo "FAIL $1 state $2: wanted \"$3\""
        echo "     got: $line"
        failed=1
        ;;
    esac
}

cat > "$SANDBOX/share.keys" <<'KEYS'
:tabnew b<CR>
state
<C-w>v
ggIX<Esc>
:tabnew a<CR>
state
<C-w>h
state
u
<C-w>l
gT
state
:q<CR>
state
:tabreopen<CR>
state
:q<CR>
:q<CR>
state
:tabclose<CR>
:tabclose<CR>
state
KEYS
run share
# A new tab after the active one, in this pane.
expect share 1 'pane 1/1 | a.md >b.md |'
# A split opens on the note; its own tabs.
expect share 2 'pane 2/2 | b.md >a.md | [a.md >b.md]'
# The same note in two panes is one buffer...
expect share 3 'pane 1/2 | a.md >b.md |'
expect share 3 'text Xbeta'
# ...with one undo history.
expect share 4 'pane 2/2 | >b.md a.md'
expect share 4 'text beta\n'
# :q closes the active tab; the pane's last used shows.
expect share 5 'pane 2/2 | >a.md |'
expect share 5 'closed 1'
expect share 6 'pane 2/2 | >b.md a.md'
# The last tab of a pane closes the pane.
expect share 7 'pane 1/1 | a.md >b.md |'
# :tabclose, and the last tab of the last pane stays.
expect share 8 'pane 1/1 | >a.md |'
expect share 8 'E784: Cannot close last tab page'

cat > "$SANDBOX/cursor.keys" <<'KEYS'
3l
:tabnew b<CR>
l
state
gT
state
<C-w>s
:tabnew b<CR>
state
KEYS
run cursor
# Each tab keeps its own cursor.
expect cursor 1 'a.md >b.md'
expect cursor 1 'cursor 1'
expect cursor 2 '>a.md b.md'
expect cursor 2 'cursor 3'
# The lower pane of an above/below split has its own tabs.
expect cursor 3 'pane 2/2 | a.md >b.md | [>a.md b.md]'

cat > "$SANDBOX/open.keys" <<'KEYS'
:tabnew<CR>
ix<Esc>
:saveas a<CR>
state
KEYS
run open
expect open 1 'E139: a.md is open in another pane'

cat > "$SANDBOX/untitled.keys" <<'KEYS'
:tabnew<CR>
i# Standup notes<CR>- one<Esc>
:q<CR>
state
:w<CR>
<CR>
state
:tabnew<CR>
ifn main() {}<Esc>
:w src/main.rs<CR>
state
:tabnew<CR>
ix<Esc>
:saveas b<CR>
state
:q!<CR>
state
KEYS
run untitled
# Text that isn't in a file needs ! to close.
expect untitled 1 'E37: No write since last change'
# :w opens the save prompt, named from the heading; ⏎ saves it.
expect untitled 2 'a.md >standup notes.md'
expect untitled 2 '"standup notes.md" written'
# Another extension saves a code file, making its folder.
expect untitled 3 '>main.rs'
# An existing name isn't replaced without !.
expect untitled 4 'E13: File exists'
# :q! drops the untitled text; the pane's last-used tab shows.
expect untitled 5 'pane 1/1 | a.md standup notes.md >main.rs |'
for file in "standup notes.md" "src/main.rs"; do
    if [ ! -f "$VAULT/$file" ]; then
        echo "FAIL untitled: $file wasn't written"
        failed=1
    fi
done
grep -q '^# Standup notes' "$VAULT/standup notes.md" 2>/dev/null || {
    echo "FAIL untitled: standup notes.md has the wrong text"
    failed=1
}

cat > "$SANDBOX/keys.keys" <<'KEYS'
key <D-t>
state
gT
state
gt
state
2gt
state
g<Tab>
state
key <D-S-]>
key <C-Tab>
state
key <C-PageUp>
state
:e b<CR>
state
<C-w>T
state
<C-w>h
:tabnew b<CR>
:tabmove 0<CR>
state
:tabmove<CR>
state
key <D-1>
state
key <D-9>
state
key <D-w>
state
key <D-S-t>
state
key <D-t>
ii<Esc>
key <D-w>
state
discard<CR>
state
<C-w>l
key <D-w>
state
:tabonly!<CR>
key <D-w>
state
KEYS
run keys
# ⌘T: an untitled tab after the active one.
expect keys 1 'pane 1/1 | a.md >untitled |'
# gT, gt, {N}gt, g⇥ (the last used).
expect keys 2 '| >a.md untitled |'
expect keys 3 '| a.md >untitled |'
expect keys 4 '| a.md >untitled |'
expect keys 5 '| >a.md untitled |'
# ⌘⇧] then ⌃⇥ wrap round; ⌃PageUp goes back.
expect keys 6 '| >a.md untitled |'
expect keys 7 '| a.md >untitled |'
# A note opened over a blank untitled tab takes its place.
expect keys 8 'pane 1/1 | a.md >b.md |'
# ⌃w T: the active tab leaves for a new pane on the right.
expect keys 9 'pane 2/2 | >b.md | [>a.md]'
# :tabmove 0 makes it first, :tabmove last.
expect keys 10 'pane 1/2 | >b.md a.md |'
expect keys 11 'pane 1/2 | a.md >b.md |'
# ⌘1 and ⌘9.
expect keys 12 '| >a.md b.md |'
expect keys 13 '| a.md >b.md |'
# ⌘W closes the tab; ⌘⇧T reopens it where it was.
expect keys 14 'pane 1/2 | >a.md |'
expect keys 14 'closed 1'
expect keys 15 'pane 1/2 | a.md >b.md |'
# Untitled text asks before ⌘W closes it; Discard closes it.
expect keys 16 '>untitled 2'
expect keys 16 'overlay open'
expect keys 17 'pane 1/2 | a.md >b.md |'
expect keys 17 'overlay none'
# ⌘W on a pane's last tab closes the pane; never mNote.
expect keys 18 'pane 1/1 | a.md >b.md |'
expect keys 19 'pane 1/1 | >b.md |'
expect keys 19 'reply ⌘Q quits mNote'

cat > "$SANDBOX/drag.keys" <<'KEYS'
:tabnew b<CR>
:tabnew a2<CR>
drag 1 3 1 1
state
<C-w>v
:e b<CR>
drag 1 2 2 end
state
drag 2 2 1 end
state
<C-w>l
:tabonly<CR>
drag 2 1 1 1
state
KEYS
printf 'alpha two\n' > "$VAULT/a2.md"
run drag
# Dropped on tab 1 of its row: it goes first.
expect drag 1 'pane 1/1 | >a2.md a.md b.md |'
# Onto another pane's row, at the end: it leaves its pane and shows there.
expect drag 2 'pane 2/2 | a2.md b.md >a.md | [>a2.md b.md]'
# Onto a pane that already has the note: that tab shows; the dragged goes.
expect drag 3 'pane 1/2 | a2.md >b.md | [a2.md >a.md]'
# A pane's last tab dragged away: the pane goes with it.
expect drag 4 'pane 1/1 | >a.md a2.md b.md |'

cat > "$SANDBOX/bulk.keys" <<'KEYS'
:tabnew b<CR>
:tabnew a2<CR>
:tabfirst<CR>
cmd tab-close-right
state
:tabnew b<CR>
:tabnew a2<CR>
:tabprevious<CR>
cmd tab-only
state
:tabnew<CR>
ix<Esc>
cmd tab-close-saved
state
cmd pane-close-tabs
state
discard<CR>
state
<C-w>.
state
jjjjj<CR>
state
KEYS
run bulk
# Close to the right, close others.
expect bulk 1 'pane 1/1 | >a.md |'
expect bulk 2 'pane 1/1 | >b.md |'
# Close saved keeps the untitled text.
expect bulk 3 'pane 1/1 | >untitled |'
# Close all asks once about text that isn't in a file...
expect bulk 4 'overlay open'
# ...and the last pane keeps a fresh untitled tab.
expect bulk 5 'pane 1/1 | >untitled 2 |'
# ⌃w . opens the menu; j j j j j ⏎ runs its sixth item, Split right.
expect bulk 6 'overlay menu'
expect bulk 7 'pane 2/2 |'
expect bulk 7 'overlay none'

if [ "$failed" = 0 ]; then
    echo "tabs: all checks passed"
fi
exit "$failed"
