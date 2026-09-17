# Development standards

## Source comments

Code should carry the reason for a surprising choice, a short public API
summary, or a citation to the relevant design document. Narrating what a line
does belongs in clearer code instead. Longer decisions belong in this
documentation set, where they can be reviewed once and updated without copying
the same argument across call sites.

`./scripts/check-comments.sh` measures whole-line comments separately for Rust,
JavaScript, shell and PowerShell, and CSS. Each group must stay at or below 10%
of its nonblank source lines. The current tree is already below that ceiling.
New work should aim for 3% to 5%, leaving room for API summaries and the few
runtime traps that are expensive to rediscover.

The documentation gate runs this check in local validation and CI. Comments
that need more than a sentence should usually become a focused section in
[Architecture](ARCHITECTURE.md), [Operations](OPERATIONS.md), or the subsystem
document that owns the decision.

## Release validation

The release workflow checks out the requested tag with full history and proves
that the tag resolves to the checked-out commit. Publication depends on three
validation jobs: the complete Linux gate with a live disposable MariaDB, the
Windows workspace gate, and the desktop/mobile browser accessibility suite.
The artifact job cannot start unless all three succeed, for tag pushes and
manual workflow runs alike.
