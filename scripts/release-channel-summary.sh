#!/bin/bash
set -euo pipefail
if [ "$#" -ne 4 ]; then
    echo "usage: $0 RELEASE_OUTCOME TAP_OUTCOME WINGET_OUTCOME AUR_OUTCOME" >&2
    exit 2
fi
release=$1
state() {
    case "$1" in
        success) echo published ;;
        failure) echo FAILED ;;
        cancelled) echo cancelled ;;
        *)
            if [ "$release" != success ]; then echo 'not run (earlier step failed or cancelled)'
            else echo 'skipped (no secret)'; fi ;;
    esac
}
{
    echo '### Publish channels'
    echo '| Channel | Result |'
    echo '|---|---|'
    echo "| GitHub Release | $(state "$release") |"
    echo '| Docker (ghcr.io) | pushed by prerequisite Docker job |'
    echo "| Homebrew tap | $(state "$2") |"
    echo "| winget | $(state "$3") |"
    echo "| AUR | $(state "$4") |"
} >> "${GITHUB_STEP_SUMMARY:?}"
for outcome in "$@"; do
    case "$outcome" in failure|cancelled) exit 1 ;; esac
done
test "$release" = success
