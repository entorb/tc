#!/bin/sh
# README screenshots (800x600) of tc's main screens into screenshots/ (Linux; needs what ui_shot.sh needs).
# usage: scripts/screenshots.sh [panel|editor|ncdu|top|rename|find|sync]   (no argument: all)
set -e
cd "$(dirname "$0")/.."
mkdir -p screenshots
# clean config: same panels, sort and zoom on every run
XDG_CONFIG_HOME=$(mktemp -d)
trap 'rm -rf "$XDG_CONFIG_HOME"' EXIT
export XDG_CONFIG_HOME TC_SHOT_SIZE=800x600
want=${1:-all}
case $want in all | panel | editor | ncdu | top | rename | find | sync) ;; *)
  echo "unknown screenshot: $want" >&2
  exit 1
  ;;
esac
w1="wait 1"
# one tc run per screenshot; steps are word-split (no spaces inside "type")
# never F10 in the panel view (quits) and never Enter in multi-rename (renames)
shot() {
  name=$1
  [ "$want" = all ] || [ "$want" = "$name" ] || return 0
  shift
  scripts/ui_shot.sh "key Tab" "type scripts" "key Return" "key Tab" "$@" "shot screenshots/$name.png"
}
shot panel
shot editor "type readme" "key F4" "$w1"
shot ncdu "key ctrl+space" "wait 5"
shot top "key ctrl+p" "wait 3" "key slash" "$w1" "type tc" "$w1" "mousemove 137 372 click 1" "wait 2" "key slash" "$w1"
shot rename "key Tab" "type +" "$w1" "type chk_*.sh" "$w1" "key Return" "$w1" "key ctrl+m" "$w1" "key F4" \
  "type %a_%b" "key Tab" "type %a" "key space" "type -" "key space" "type %b" "$w1"
# find: name mask, then the Content tab
shot find "key ctrl+f" "$w1" "type *.sh" "key F3" "type cargo" "$w1"
# sync: tc against scripts, compared
shot sync "key ctrl+e" "wait 3"
