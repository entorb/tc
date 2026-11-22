# Code Maintenance Prompts

## Code Review UI

- perform a UI code review
- use codegraph (init if needed) and context7
- focus on
  - F1 help up to date and human readable
  - ensure UI is intuitive
  - feature completeness, suggest improvements
  - check check code of templates/mc, doublecmd, yazi (use codegraph) if they have a similar feature
  - suggest improvements to the UI
  - suggest harminisations of feature UIs to ensure consistence across app (F10 close, etc.)
    - editor/viewer
    - ncdu
    - top
    - multi-rename
    - search file
    - ...
  - in tc-gui use icons, ensure they are consistent

## Code Review Performance

- perform a code review
- use codegraph and context7
- focus on
  - potential bugs
  - performance
  - security
  - best practice of 2026

## Code Review Dependencies

- perform a code review
- use codegraph and context7
- focus on
  - which external packages are used
  - are they actively maintained
  - do they have successors
  - can code be replaced by already added packages?
  - should code be replaced by new packages?
  - follow ponytail rules

## Merge commits touching same feature

- check git history after last tag
- squash merge commits that touch the same feature to keep history clean

## Review and compress README.md and AGENTS.md files

- README.md shall list all features, but as overview, details should maybe moved to F1 help screens.
- AGENTS.md shall help to ensure that ai coding is most performant and known issues are prevented
