#!/bin/bash
# Compares Volant with the other browsers installed on this Mac (GitHub's
# macOS runners have Safari, Chrome and Firefox). Writes bench/results.md.
#
#   launch   time from starting the app to its first window on screen (median of 3)
#   memory   memory footprint (as in Activity Monitor) summed over every process the
#            browser started (app + web content, GPU and network helpers), same 5 pages
#   idle CPU CPU time those processes used over 30 s with nothing happening
#   size     the .app on disk (Safari and Volant also use the WebKit built into macOS)
set -uo pipefail
cd "$(dirname "$0")/../.."
OUT=bench
mkdir -p "$OUT"
swiftc -O scripts/bench/launch.swift -o "$OUT/launch" 2>/dev/null || { echo "swiftc failed"; exit 1; }

URLS=(https://example.com/ https://en.wikipedia.org/wiki/Web_browser https://www.rust-lang.org/ https://news.ycombinator.com/ https://developer.apple.com/documentation/webkit)
SETTLE=25
IDLE=30
TMP="${RUNNER_TEMP:-/tmp}/volant-bench"
rm -rf "$TMP"; mkdir -p "$TMP"

pids_now() { ps -axo pid= | tr -d ' ' | sort; }

# Memory footprint (MB, what Activity Monitor shows) and CPU seconds of the
# processes started since $1 (a pid list file) whose command matches $2.
measure() {
  python3 - "$1" "$2" <<'PY'
import re, subprocess, sys
before = set(open(sys.argv[1]).read().split())
pat = re.compile(sys.argv[2], re.I)
procs = {}
for line in subprocess.run(["ps", "-axo", "pid=,time=,comm="], capture_output=True, text=True).stdout.splitlines():
    pid, t, comm = line.strip().split(None, 2)
    if pid in before or not pat.search(comm):
        continue
    d = 0
    if "-" in t:
        d, t = t.split("-"); d = int(d)
    parts = [float(x) for x in t.split(":")]
    while len(parts) < 3:
        parts.insert(0, 0.0)
    procs[pid] = d * 86400 + parts[0] * 3600 + parts[1] * 60 + parts[2]
mem = 0.0
if procs:
    out = subprocess.run(["top", "-l", "1", "-stats", "pid,mem"], capture_output=True, text=True).stdout
    units = {"B": 1 / 1048576, "K": 1 / 1024, "M": 1, "G": 1024}
    for line in out.splitlines():
        m = re.match(r"\s*(\d+)\s+([\d.]+)([BKMG])", line)
        if m and m.group(1) in procs:
            mem += float(m.group(2)) * units[m.group(3)]
print(f"{mem:.0f} {sum(procs.values()):.2f} {len(procs)}")
PY
}

quit_all() {
  pkill -x Volant 2>/dev/null; osascript -e 'quit app "Safari"' 2>/dev/null; pkill -x Safari 2>/dev/null
  pkill -f "Google Chrome" 2>/dev/null; pkill -f "Firefox.app" 2>/dev/null
  sleep 4
}

# Per-browser details (plain functions: macOS ships bash 3.2, which has no maps).
app_of() { case "$1" in Volant) echo "dist/Volant.app";; Safari) echo "/Applications/Safari.app";; Chrome) echo "/Applications/Google Chrome.app";; Firefox) echo "/Applications/Firefox.app";; esac; }
exe_of() { case "$1" in Volant) echo "dist/Volant.app/Contents/MacOS/Volant";; Safari) echo "/usr/bin/open";; Chrome) echo "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";; Firefox) echo "/Applications/Firefox.app/Contents/MacOS/firefox";; esac; }
owner_of() { case "$1" in Chrome) echo "Google Chrome";; *) echo "$1";; esac; }
pattern_of() { case "$1" in Volant) echo 'Volant|com\.apple\.WebKit';; Safari) echo 'Safari|com\.apple\.WebKit';; Chrome) echo 'Google Chrome';; Firefox) echo 'firefox|plugin-container|Firefox';; esac; }

args_for() {
  local b="$1"; shift
  case "$b" in
    Volant)
      mkdir -p "$TMP/volant"; printf 'onboarded=1\nrestore_session=0\n' > "$TMP/volant/settings.txt"
      echo "$@" ;;
    Safari) echo "-a Safari $*" ;;
    Chrome) echo "--user-data-dir=$TMP/chrome --no-first-run --no-default-browser-check --disable-search-engine-choice-screen $*" ;;
    Firefox)
      mkdir -p "$TMP/firefox"
      cat > "$TMP/firefox/user.js" <<'JS'
user_pref("browser.aboutwelcome.enabled", false);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);
user_pref("trailhead.firstrun.didSeeAboutWelcome", true);
JS
      echo "-profile $TMP/firefox -no-remote $*" ;;
  esac
}

{
  echo "| Browser | App size | Launch to window | Memory, 5 pages | Processes | Idle CPU (30 s) |"
  echo "| --- | --- | --- | --- | --- | --- |"
} > "$OUT/table.md"

for b in Volant Safari Chrome Firefox; do
  [ -e "$(app_of "$b")" ] || { echo "skip $b (not installed)"; continue; }
  echo "== $b"
  size=$(du -sm "$(app_of "$b")" | cut -f1)
  quit_all
  export VOLANT_DATA_DIR="$TMP/volant"
  # Launch time: one warm-up run, then the median of three.
  times=()
  for i in 0 1 2 3; do
    t=$("$OUT/launch" "$(owner_of "$b")" "$(exe_of "$b")" $(args_for "$b" https://example.com/))
    echo "  launch $i: $t ms"
    [ "$i" -gt 0 ] && times+=("$t")
    quit_all
  done
  launch=$(printf '%s\n' "${times[@]}" | sort -n | sed -n 2p)

  # Memory and idle CPU with the same five pages open.
  pids_now > "$OUT/before.txt"
  "$OUT/launch" "$(owner_of "$b")" "$(exe_of "$b")" $(args_for "$b" "${URLS[@]}") > /dev/null
  sleep "$SETTLE"
  read -r mem0 cpu0 n < <(measure "$OUT/before.txt" "$(pattern_of "$b")")
  sleep "$IDLE"
  read -r mem1 cpu1 n2 < <(measure "$OUT/before.txt" "$(pattern_of "$b")")
  idle=$(python3 -c "print(f'{max(0, $cpu1 - $cpu0):.2f} s')")
  mem="${mem1} MB"
  echo "  size ${size} MB, launch ${launch} ms, memory $mem in $n2 processes, idle CPU $idle"
  echo "| $b | ${size} MB | ${launch} ms | $mem | $n2 | $idle |" >> "$OUT/table.md"
  quit_all
done

{
  echo "## Volant vs other browsers"
  echo
  echo "Measured on a GitHub Actions macOS runner ($(sw_vers -productVersion), $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.memsize | awk '{print $1/1073741824}') GB)."
  echo "Pages: ${URLS[*]}"
  echo
  cat "$OUT/table.md"
  echo
  echo "Memory is each process's memory footprint (the figure Activity Monitor shows), summed over every process the browser started: app, web content, GPU and networking helpers."
  echo "Safari and Volant render with the WebKit built into macOS, so their app sizes don't include an engine; Chrome and Firefox ship their own."
} > "$OUT/results.md"
cat "$OUT/results.md"
