# TODOs

## Feature XXX

- check check code templates/mc, doublecmd, yazi (use codegraph (init if needed)) if the have a similar feature
- add to toolbar, help, menu, add shortcut

---

## crates/tc-cli/src/app.rs

- crates/tc-cli/src/app.rs is huge, should it be refactored into smaller files?
- file path is a bit confusing: tc-cli sounds als it is cli only code, but most of is is shared between tc-cli and tc-gui, maybe move it to a shared package?
