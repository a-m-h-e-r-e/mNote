#!/bin/sh
# Keystroke latency on a large note (336 KB of long lines), wrapped and not.
# Types 300 keys into a release build in a throwaway HOME (your own notes
# and config are never touched) and fails if mNote's own work per key goes
# over the 8 ms budget at p95.
#
#   tools/bench-large-note.sh
#
# Needs a display: it opens a window for each run.
set -eu

cd "$(dirname "$0")/.."
cargo build --release -p mnote-app
BIN="$PWD/target/release/mnote"
SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/home/.config/mnote" "$SANDBOX/home/notes"
NOTE="$SANDBOX/home/notes/large.md"

# The same note every time: 300 lines of 50 to 400 words.
python3 -I - "$NOTE" <<'PY'
import random, sys
random.seed(1)
words = "the quick brown fox jumps over a lazy dog while code and notes keep moving along".split()
lines = ["# Large note"] + [
    " ".join(random.choice(words) for _ in range(random.randint(50, 400)))
    for _ in range(300)
]
open(sys.argv[1], "w").write("\n".join(lines) + "\n")
PY
cp "$NOTE" "$SANDBOX/large.md"

status=0
for wrap in true false; do
    cp "$SANDBOX/large.md" "$NOTE"
    printf 'vault = "%s"\nwrap = %s\n' "$SANDBOX/home/notes" "$wrap" \
        > "$SANDBOX/home/.config/mnote/config.toml"
    work=$(HOME="$SANDBOX/home" XDG_CONFIG_HOME="$SANDBOX/home/.config" \
        MNOTE_LATENCY=1 MNOTE_LATENCY_BENCH=300 "$BIN" "$NOTE" 2>&1 | grep "mNote's work" || true)
    echo "wrap = $wrap: ${work#*wait): }"
    p95=$(echo "$work" | sed -n 's/.*p95 \([0-9.]*\) ms.*/\1/p')
    if [ -z "$p95" ] || [ "$(echo "$p95 > 8" | bc)" = 1 ]; then
        echo "  over the 8 ms budget (or no result)"
        status=1
    fi
done
# A 1 MB Rust file, opened as code: keystrokes within the budget, and its
# first colours within 50 ms of opening.
CODE="$SANDBOX/home/notes/large.rs"
python3 -I - "$CODE" <<'PY'
import sys
line = "    pub fn example(&self, value: u32) -> Option<String> { let s = format!(\"{value}\"); Some(s) } // note\n"
open(sys.argv[1], "w").write("pub struct Example;\n\nimpl Example {\n" + line * 10000 + "}\n")
PY
printf 'vault = "%s"\nwrap = false\n' "$SANDBOX/home/notes" > "$SANDBOX/home/.config/mnote/config.toml"
out=$(HOME="$SANDBOX/home" XDG_CONFIG_HOME="$SANDBOX/home/.config" \
    MNOTE_LATENCY=1 MNOTE_LATENCY_BENCH=300 "$BIN" "$CODE" 2>&1 || true)
work=$(echo "$out" | grep "mNote's work" || true)
echo "rust 1 MB: ${work#*wait): }"
p95=$(echo "$work" | sed -n 's/.*p95 \([0-9.]*\) ms.*/\1/p')
if [ -z "$p95" ] || [ "$(echo "$p95 > 8" | bc)" = 1 ]; then
    echo "  over the 8 ms budget (or no result)"
    status=1
fi
colours=$(echo "$out" | sed -n 's/^mnote first colours: \([0-9.]*\) ms.*/\1/p' | head -n 1)
echo "rust 1 MB: first colours ${colours:-?} ms"
if [ -z "$colours" ] || [ "$(echo "$colours > 50" | bc)" = 1 ]; then
    echo "  over 50 ms to first colours (or no result)"
    status=1
fi
exit $status
