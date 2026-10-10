#!/usr/bin/env bash
# Sync release_fork with upstream main.
# It NEVER pushes: review the result, then push manually.
#
# Usage: ./sync-upstream.sh
set -euo pipefail

UPSTREAM_URL="https://github.com/niri-wm/niri.git"
BRANCH="release_fork"

die() { echo "sync-upstream: $*" >&2; exit 1; }

# --- 0. sanity ---------------------------------------------------------------
[[ "$(git branch --show-current)" == "$BRANCH" ]] || die "not on $BRANCH, abort"
[[ -z "$(git status --porcelain)" ]] || die "worktree dirty, commit or stash first"

# --- 1. upstream -------------------------------------------------------------
git remote get-url upstream >/dev/null 2>&1 \
  || { echo "adding upstream remote"; git remote add upstream "$UPSTREAM_URL"; }
git fetch upstream

if git merge-base --is-ancestor upstream/main HEAD; then
  echo "already up to date with upstream/main"
else
  echo "== new upstream commits =="
  git log --oneline "HEAD..upstream/main"
fi

# --- 2. rebase ----------------------------------------------------------------
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse upstream/main)" ]]; then
  echo "== rebasing $BRANCH onto upstream/main =="
  if ! git rebase upstream/main; then
    echo "conflict during rebase, aborting (your branch is untouched)" >&2
    git rebase --abort
    exit 1
  fi
else
  echo "branch already on upstream/main tip, no rebase needed"
fi

# --- 3. summary -----------------------------------------------------------------
echo
echo "== result =="
git log --oneline -3
git status --short
echo
echo "next (manual): review, then push:"
echo "  git push --force-with-lease origin $BRANCH"
echo "then in dotfiles/nixos: nix flake update niri && rebuild"
