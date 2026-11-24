#!/bin/sh
# Screenshot tc headless for UI checks (Linux; needs xvfb, xdotool, imagemagick, libxkbcommon-x11-0).
# usage: scripts/ui_shot.sh STEP...
#   STEP = "shot FILE.png" | "wait SECONDS" | "key COMBO..." (ui_key.py: F1 ctrl+f shift+F4 Down Return)
#        | any other xdotool command: "type abc", "mousemove 300 300 click 1"
# example: scripts/ui_shot.sh "shot /tmp/main.png" "key F9" "shot /tmp/menu.png"
# tc opens in the caller's directory with the user's config; window 1280x800 (TC_SHOT_SIZE=WxH to change).
set -e
root="$(dirname "$0")/.."
bin=$(cargo build --manifest-path "$root/Cargo.toml" -p tc --message-format=json 2>/dev/null |
  grep -o '"executable":"[^"]*/tc"' | cut -d'"' -f4)
[ -x "$bin" ] || {
  echo "build failed: cargo build -p tc" >&2
  exit 1
}
TC_SHOT_DIR="$(cd "$(dirname "$0")" && pwd)"
export TC_SHOT_BIN="$bin" TC_SHOT_DIR
# shellcheck disable=SC2016 # the inner script expands its variables inside Xvfb
exec xvfb-run -a -s "-screen 0 ${TC_SHOT_SIZE:-1280x800}x24" sh -c '
  LIBGL_ALWAYS_SOFTWARE=1 "$TC_SHOT_BIN" >/dev/null 2>&1 &
  pid=$!
  trap "kill $pid 2>/dev/null" EXIT
  w=""
  for _ in $(seq 50); do
    w=$(xdotool search --pid $pid 2>/dev/null | tail -1) && [ -n "$w" ] && break
    sleep 0.2
  done
  [ -n "$w" ] || { echo "tc window did not appear" >&2; exit 1; }
  sleep 1
  # no window manager: focus by hand so keys reach tc
  xdotool windowfocus "$w"
  for step in "$@"; do
    case $step in
      "shot "*) import -window root "${step#shot }" ;;
      "wait "*) sleep "${step#wait }" ;;
      # xdotool key adds Alt to F-keys on Xvfb (VT-switch levels), so keys go through XTEST
      "key "*) python3 "$TC_SHOT_DIR/ui_key.py" ${step#key }; sleep 0.5 ;;
      *) xdotool $step; sleep 0.5 ;;
    esac
  done
' sh "$@"
