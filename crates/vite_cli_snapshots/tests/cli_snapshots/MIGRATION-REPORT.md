# Snap-test migration report

Source: `packages/cli/snap-tests` (flavor: local), 4 case(s).

Record baselines with `UPDATE_SNAPSHOTS=1 just snapshot-test <filter>`,
review each new snapshot against the old snap.txt, then delete the old
case directories in the same PR.

## check-pass

- note: renamed to `check_pass` (identifier rule)

## check-pass-no-typecheck

- note: renamed to `check_pass_no_typecheck` (identifier rule)

## check-pass-typecheck

- note: renamed to `check_pass_typecheck` (identifier rule)

## check-pass-typecheck-github-actions

- note: renamed to `check_pass_typecheck_github_actions` (identifier rule)
