# IDEAS

- Properties/chmod/chown (Unix) + attributes column.

## Git support

- if in git repo, show .gitignoreed files/dirs in lighter color

## Move Window Bug

- Known issue, Windows 11: the window cannot move to a screen with a different display scale % (mouse or Win+Shift+Arrow). Upstream winit 0.30 bug ([winit#4694](https://github.com/rust-windowing/winit/pull/4694)); fixed by a winit / eframe update. Workaround: same scale % on all screens.
winit 0.30.13 is the newest stable release and eframe 0.37 uses it
- Fixed on winit master in [#4341](https://github.com/rust-windowing/winit/pull/4341), but only for winit 0.31
- Fix for 0.30 still open: [#4694](https://github.com/rust-windowing/winit/pull/4694)
- TODO: update package when fix is available
