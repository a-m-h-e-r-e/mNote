#!/bin/sh
# Cold start with a restored session (AMI-223): 3 panes × 8 tabs on 24 notes
# of about 20 KB. A first run makes the panes and tabs and quits, which
# writes the session; then mNote starts five times restoring it, and five
# without, in a throwaway HOME (your own notes and config are never
# touched). It reports the median start (from the process starting to the
# first frame) against the 200 ms budget, and fails if restoring the
# session costs more than 20 ms of it.
#
#   tools/bench-start.sh
#
# Needs a display: it opens a window for each run.
set -eu

cd "$(dirname "$0")/.."
cargo build --release -p mnote-app
BIN="$PWD/target/release/mnote"
SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT
VAULT="$SANDBOX/home/notes"
CONFIG="$SANDBOX/home/.config/mnote/config.toml"
mkdir -p "$SANDBOX/home/.config/mnote" "$VAULT"

python3 -I - "$VAULT" <<'PY'
import sys
words = "the quick brown fox jumps over a lazy dog while notes keep moving along".split()
for n in range(1, 25):
    lines = [f"# Note {n}", ""] + [
        " ".join(words[(n + i + j) % len(words)] for j in range(12)) for i in range(300)
    ]
    open(f"{sys.argv[1]}/n{n}.md", "w").write("\n".join(lines) + "\n")
open(f"{sys.argv[1]}/inbox.md", "w").write("# Inbox\n")
PY

# play SCRIPT RESTORE: runs mNote on the vault, printing its timings.
play() {
    printf 'vault = "%s"\nrestore_session = %s\n' "$VAULT" "$2" > "$CONFIG"
    HOME="$SANDBOX/home" XDG_CONFIG_HOME="$SANDBOX/home/.config" \
        MNOTE_LATENCY=1 MNOTE_SCRIPT="$1" "$BIN" 2>&1 | grep '^mnote \(start\|restore\)' || true
}

# The panes and tabs: n1…n8 | (n9…n16 over n17…n24).
{
    for n in 1 2 3 4 5 6 7 8; do echo ":e n$n<CR>"; done
    echo "<C-w>v"
    for n in 9 10 11 12 13 14 15 16; do echo ":e n$n<CR>"; done
    echo "<C-w>s"
    for n in 17 18 19 20 21 22 23 24; do echo ":e n$n<CR>"; done
} > "$SANDBOX/setup.keys"
printf 'wait 300\n' > "$SANDBOX/idle.keys"
play "$SANDBOX/setup.keys" true > /dev/null

# median FILE: the median of the numbers in FILE.
median() {
    sort -n "$1" | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'
}

for restore in true false; do
    : > "$SANDBOX/start-$restore"
    : > "$SANDBOX/restore-$restore"
    for _ in 1 2 3 4 5; do
        out=$(play "$SANDBOX/idle.keys" "$restore")
        echo "$out" | sed -n 's/^mnote start: \([0-9.]*\) ms/\1/p' >> "$SANDBOX/start-$restore"
        echo "$out" | sed -n 's/^mnote restore: \([0-9.]*\) ms/\1/p' >> "$SANDBOX/restore-$restore"
    done
done
with=$(median "$SANDBOX/start-true")
without=$(median "$SANDBOX/start-false")
restore=$(median "$SANDBOX/restore-true")
echo "start, 3 panes × 8 tabs restored: ${with:-?} ms (restoring ${restore:-?} ms)"
echo "start, nothing restored: ${without:-?} ms"
if [ -z "$with" ] || [ -z "$without" ]; then
    echo "  no timings (did the window open?)"
    exit 1
fi
if awk "BEGIN { exit !($with > 200) }"; then
    echo "  note: start is over the 200 ms budget; $without ms of it is with nothing restored"
fi
if awk "BEGIN { exit !($with - $without > 20 || ${restore:-99} > 20) }"; then
    echo "  restoring the session costs more than 20 ms of the start"
    exit 1
fi
