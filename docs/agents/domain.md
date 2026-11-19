# Domain docs

## Before exploring

Read the root `GLOSSARY.md` when present.
Read relevant architecture decisions under `docs/adr/`.

If either is absent, proceed without requesting scaffolding.
Create domain docs lazily when terms or decisions are resolved.

## Vocabulary and decisions

Use glossary terms consistently in specs, tickets, tests, and code.
Read `docs/layout.md` for the accepted Worker, Gateway, and independent Client structure. Its architecture decision supersedes conflicting scaffold paths and SDK naming in the source proposal.
Proposal revision 1.1 still distinguishes executable Worker Packages from user-owned Runtime Profiles. It does not override later user-approved architecture decisions.

Surface conflicts with existing ADRs rather than silently overriding them.
An ADR inventory in the proposal does not make every listed proposal approved.
