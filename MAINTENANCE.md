# Code Maintenance Prompts

Run monthly, after many new features. Order: squash → performance → UI → dependencies → docs.

For every review:

- full review, focus on changes since last tag
- report findings ranked by severity (bug / risk / nit) with `file:line`, top 10 max; fix only what I pick
- finish with `scripts/run_checks.sh`; add recurring pitfalls to AGENTS.md

## Merge commits touching same feature

- check git history after last tag
- squash commits that touch the same feature to keep history cleaner
- only unpushed commits (`git log origin/main..HEAD`); keep `*.md` commits separate
- show plan (groups + new subjects), wait for approval

## Code Review Performance

- perform a code review
- focus on
  - potential bugs
  - data safety of file ops (overwrite, partial targets, cleanup)
  - performance
  - security
  - best practice of 2026
  - open Sonar issues (`scripts/get_sonar_issues.sh`)

## Code Review UI

- perform a UI code review
- use codegraph (init if needed) and context7
- focus on
  - F1 help up to date and human readable
  - ensure UI is intuitive
  - feature completeness, suggest improvements
  - check `templates/FEATURES-*.md` first, then code of templates/mc, doublecmd, krusader, yazi (use codegraph) if they have a similar feature
  - suggest harmonisations of feature UIs to ensure consistency across app (F10 close, F1 help, footer keys, F9 menu entry, etc.)
    - editor/viewer
    - File Stats (ncdu)
    - Processes (top)
    - multi-rename
    - search file
    - ...
  - use icons, ensure they are consistent (same icon for same action)

## Code Review: Dependencies

- perform a code review
- use codegraph (init if needed) and context7
- focus on
  - which external packages are used
  - are they actively maintained
  - do they have successors
  - can code be replaced by already added packages?
  - should code be replaced by new packages?
  - duplicate versions (`cargo tree -d`)
  - follow ponytail rules

## Review and compress README.md and AGENTS.md files

- README.md shall list all features, but as overview, details should maybe moved to F1 help screens.
- AGENTS.md (root and `crates/*/AGENTS.md`) shall help to ensure that AI coding is most performant and known issues are prevented
- remove duplicates between AGENTS.md files; verify referenced files, scripts and tests still exist
