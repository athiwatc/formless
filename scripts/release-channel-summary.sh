#!/bin/bash
set -euo pipefail
if [ "$#" -ne 5 ]; then
    echo "usage: $0 RELEASE_OUTCOME TAP_OUTCOME WINGET_OUTCOME AUR_OUTCOME DOCKER_OUTCOME" >&2
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
docker_state() {
    case "$1" in
        success)
            case "${DOCKER_LATEST:-}" in
                promoted) echo 'published (latest promoted)' ;;
                'skipped (not the highest stable tag)') echo 'published (latest skipped: not the highest stable tag)' ;;
                *) echo 'published' ;;
            esac ;;
        failure|cancelled) state "$1" ;;
        *) echo 'not run (required publication steps did not complete)' ;;
    esac
}
{
    echo '### Publish channels'
    echo '| Channel | Result |'
    echo '|---|---|'
    echo "| GitHub Release | $(state "$release") |"
    echo "| Docker (ghcr.io) | $(docker_state "$5") |"
    echo "| Homebrew tap | $(state "$2") |"
    echo "| winget | $(state "$3") |"
    echo "| AUR | $(state "$4") |"
} >> "${GITHUB_STEP_SUMMARY:?}"
for outcome in "$@"; do
    case "$outcome" in failure|cancelled) exit 1 ;; esac
done
test "$release" = success && test "$5" = success
