# TODOs

---

## Feedback windows

[ ] Create proper App icon, maybe a floppy disk
[ ] multi-screen with different scale %: window cannot move to other screen (winit 0.30.13 bug). Bump winit when 0.30.14 ships (backport PR rust-windowing/winit#4694) or eframe moves to winit 0.31 (fix #4341); then drop README known issue
[ ] copy error on windows: Error C:\tmp\ot-retro\node_modules\.pnpm\@asamuzakjp+css-color@7.1.9\node_modules\@csstools\css-calc: A required privilege is not held by the client. (os error 1314) OK Esc
[ ] copy multi-threading to handle huge number of files
[ ] DEL key -> F8
[ ] F1 help shall use full width of the popup, currently the scollbar is at approx 2/3 of the width. multi rename help mouse wheel does not work. use standard comonents
[ ] F1 help: no obvious navigation into like in multi rename: Fields Tab Shift+Tab Up/Down Space Click Left/Right Alt+Up/Down PgUp/PgDn next field previous field previous / next field toggle checkbox, cycle choice focus a field; toggles a checkbox, cycles a choice move in text, cycle choice earlier / later settings (history) scroll the preview
[ ] in Right panel on dir, CTRL+Left: open current selected dir in left panel, and vice versa
[ ] ncdu windows: high cpu 90% if using mouse wheel
[ ] ncdu: if filter is active: display icon on the respecrive table column header
[ ] ncdu: table columns have virtual offsets
[ ] panels: click into panel shall set focus to panel. currently only wirks for click on file/dir
[ ] releases: binaries in .zip to keep simple binary name e.g. tc while zip is tc-Linux-amd64.zip and to unblock .exe downloads in Edge
[ ] top right of panel, add button for open history of panel dirs. left of it, add button to open/edit/"add current dir" bookmarks. remove bookmark icon from toolbar
[ ] Bookmark: after adding single entry the selection is flashing constantly, like it tries to press arrow down button
[ ] ensure to use standard UI components, e.g table in help, top and ncdu

---
