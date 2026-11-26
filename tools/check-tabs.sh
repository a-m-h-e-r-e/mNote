#!/bin/sh
# Tab pages end to end (AMI-218): plays key scripts into a release build in
# a throwaway HOME (your own notes and config are never touched) and checks
# the tabs, panes and text it reports, and the files it writes.
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
:tabnew a<CR>
ggIX<Esc>
state
:tabnext 1<CR>
state
u
:tabnext 3<CR>
state
:tabclose<CR>
state
:tabreopen<CR>
state
:q<CR>
state
:tabclose 2<CR>
state
:tabclose<CR>
state
KEYS
run share
# A new tab after this one, on the note named.
expect share 1 'tab 2/2 | >b.md | [>a.md]'
# The same note in two tabs is one buffer...
expect share 2 'tab 3/3 | >a.md'
expect share 2 'text Xalpha'
expect share 3 'tab 1/3 | >a.md'
expect share 3 'text Xalpha'
# ...with one undo history.
expect share 4 'tab 3/3'
expect share 4 'text alpha\n'
# Closing shows the last-used tab (tab 1, not tab 2 to the left).
expect share 5 'tab 1/2 | >a.md'
expect share 5 'closed 1'
expect share 6 'tab 3/3 | >a.md'
# :q on a tab's last pane closes the tab.
expect share 7 'tab 1/2'
expect share 8 'tab 1/1'
expect share 9 'E784: Cannot close last tab page'

cat > "$SANDBOX/panes.keys" <<'KEYS'
<C-w>v
:e b<CR>
:tabnew<CR>
state
:tabprevious<CR>
state
<C-w>w
state
:tabonly!<CR>
state
KEYS
run panes
# A tab keeps its own panes; the new one has one, on an untitled note.
expect panes 1 'tab 2/2 | >untitled | [a.md >b.md]'
expect panes 2 'tab 1/2 | a.md >b.md |'
expect panes 3 '| >a.md b.md |'
expect panes 4 'tab 1/1'

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
expect untitled 2 '>standup notes.md'
expect untitled 2 '"standup notes.md" written'
# A path with another extension saves a code file, making its folder.
expect untitled 3 '>main.rs'
# An existing name isn't replaced without !, nor a note open elsewhere.
expect untitled 4 'E13: File exists'
# :q! drops the untitled text, and the last-used tab shows.
expect untitled 5 'tab 3/3 | >main.rs'
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
<C-w>v
<C-w>T
state
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
key <D-S-w>
key <D-S-w>
key <D-S-w>
key <D-w>
state
KEYS
run keys
# ⌘T: a new tab, on an untitled note.
expect keys 1 'tab 2/2 | >untitled | [>a.md]'
# gT, gt, {N}gt, g⇥ (the last used).
expect keys 2 'tab 1/2 | >a.md'
expect keys 3 'tab 2/2'
expect keys 4 'tab 2/2'
expect keys 5 'tab 1/2'
# ⌘⇧] then ⌃⇥ wrap round; ⌃PageUp goes back.
expect keys 6 'tab 1/2'
expect keys 7 'tab 2/2'
# ⌃w T: the focused pane leaves for a tab of its own, after this one.
expect keys 8 'tab 3/3 | >b.md | [>a.md] [>b.md]'
# :tabmove 0 makes it first, :tabmove last.
expect keys 9 'tab 1/3 | >b.md | [>a.md] [>b.md]'
expect keys 10 'tab 3/3 | >b.md | [>a.md] [>b.md]'
# ⌘1 and ⌘9.
expect keys 11 'tab 1/3 | >a.md'
expect keys 12 'tab 3/3 | >b.md'
# ⌘W closes the tab (its last pane); ⌘⇧T reopens it.
expect keys 13 'tab 1/2'
expect keys 13 'closed 1'
expect keys 14 'tab 3/3 | >b.md'
# Untitled text asks before ⌘W closes it.
expect keys 15 'tab 4/4 | >untitled 2'
expect keys 15 'overlay open'
# Discard closes it, and the last-used tab shows.
expect keys 16 'tab 3/3 | >b.md'
expect keys 16 'overlay none'
# ⌘⇧W closes tabs; ⌘W on the last pane of the last tab never quits.
expect keys 17 'tab 1/1'
expect keys 17 'reply ⌘Q quits mNote'

if [ "$failed" = 0 ]; then
    echo "tabs: all checks passed"
fi
exit "$failed"
