#!/bin/sh
# Keystroke latency on a large note (336 KB of long lines), wrapped and not,
# and on a 1 MB Rust file; editing code there (AMI-242): Enter with indent
# and a bracket with auto-pairs (8 ms), closing and opening a fold of 10 000
# lines and opening the outline (16 ms); then switching tabs (AMI-224): 3
# panes × 8 tabs on 1 MB notes, `gt` 200 times. Runs a release build in a
# throwaway HOME (your own notes and config are never touched) and fails if
# mNote's own work goes over budget at p95: 8 ms per key, 16 ms for a fold,
# the outline or a tab switch.
#
#   tools/bench-large-note.sh
#
# Needs a display: it opens a window for each run. Keep the window in front
# (macOS draws a covered window less often, and a key is only timed when
# its frame is drawn): a run that times no keys fails.
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
    printf 'vault = "%s"\nwrap = %s\nrestore_session = false\n' "$SANDBOX/home/notes" "$wrap" \
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
printf 'vault = "%s"\nwrap = false\nrestore_session = false\n' "$SANDBOX/home/notes" > "$SANDBOX/home/.config/mnote/config.toml"
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

# Editing code on the 1 MB Rust file: each `bench` line times its keys
# from the first key to the frame that shows them.
{
    echo ":e large.rs<CR>"
    echo "wait 1500"
    echo "4GA"
    echo "bench 200 <CR>"
    echo "bench 200 ("
    echo "<Esc>3G"
    echo "bench 60 za"
    echo "bench 30 :outline<CR> | <Esc>"
    echo "<Esc>"
} > "$SANDBOX/code.keys"
printf 'vault = "%s"\nwrap = false\nrestore_session = false\n' "$SANDBOX/home/notes" > "$SANDBOX/home/.config/mnote/config.toml"
reports=$(HOME="$SANDBOX/home" XDG_CONFIG_HOME="$SANDBOX/home/.config" \
    MNOTE_LATENCY=1 MNOTE_SCRIPT="$SANDBOX/code.keys" "$BIN" 2>&1 |
    grep -E "latency over|mNote's work" || true)
n=0
for check in "enter with indent:8" "auto-pairs:8" "fold of 10 000 lines:16" "outline:16"; do
    n=$((n + 1))
    name=${check%:*}
    budget=${check##*:}
    # Each report is a count line, then the work line; none counted fails.
    keys=$(echo "$reports" | sed -n "$((2 * n - 1))p" | sed -n 's/.*over \([0-9]*\) keys.*/\1/p')
    work=$(echo "$reports" | sed -n "$((2 * n))p")
    echo "rust 1 MB, $name: ${work#*wait): } over ${keys:-0} keys"
    p95=$(echo "$work" | sed -n 's/.*p95 \([0-9.]*\) ms.*/\1/p')
    if [ "${keys:-0}" = 0 ] || [ -z "$p95" ] || [ "$(echo "$p95 > $budget" | bc)" = 1 ]; then
        echo "  over the $budget ms budget (or no result)"
        status=1
    fi
done

# Switching tabs: 24 notes of 1 MB in n1…n8 | (n9…n16 over n17…n24), then
# `gt` round the focused pane's 8 tabs.
python3 -I - "$SANDBOX/home/notes" <<'PY'
import sys
words = "the quick brown fox jumps over a lazy dog while notes keep moving along".split()
for n in range(1, 25):
    line = lambda i: " ".join(words[(n + i + j) % len(words)] for j in range(12))
    lines = [f"# Note {n}", ""] + [line(i) for i in range(14000)]
    open(f"{sys.argv[1]}/n{n}.md", "w").write("\n".join(lines) + "\n")
PY
{
    for n in 1 2 3 4 5 6 7 8; do echo ":e n$n<CR>"; done
    echo "<C-w>v"
    for n in 9 10 11 12 13 14 15 16; do echo ":e n$n<CR>"; done
    echo "<C-w>s"
    for n in 17 18 19 20 21 22 23 24; do echo ":e n$n<CR>"; done
    echo "wait 500"
    echo "bench 200 gt"
} > "$SANDBOX/tabs.keys"
printf 'vault = "%s"\nwrap = true\nrestore_session = false\n' "$SANDBOX/home/notes" > "$SANDBOX/home/.config/mnote/config.toml"
work=$(HOME="$SANDBOX/home" XDG_CONFIG_HOME="$SANDBOX/home/.config" \
    MNOTE_LATENCY=1 MNOTE_SCRIPT="$SANDBOX/tabs.keys" "$BIN" 2>&1 | grep "mNote's work" || true)
echo "tab switch, 3 panes × 8 tabs of 1 MB: ${work#*wait): }"
p95=$(echo "$work" | sed -n 's/.*p95 \([0-9.]*\) ms.*/\1/p')
if [ -z "$p95" ] || [ "$(echo "$p95 > 16" | bc)" = 1 ]; then
    echo "  over the 16 ms budget (or no result)"
    status=1
fi
exit $status
