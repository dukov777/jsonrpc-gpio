#!/usr/bin/env bash
# Footprint recording moved to the `prepare-commit-msg` hook
# (.claude/hooks/esp32-size-record.sh), which records each commit's row INTO
# that same commit with no `git commit --amend`. This post-commit script is now
# intentionally a no-op, kept only so the existing .git/hooks/post-commit
# wiring does nothing harmful (it must never create or amend commits).
exit 0
