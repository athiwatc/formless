#!/bin/bash
# Publish the immutable local image that passed the preceding smoke step.
set -euo pipefail
: "${RELEASE_IMAGE:?}" "${GITHUB_REF_NAME:?}"
image_id=$(docker image inspect --format '{{.Id}}' "$RELEASE_IMAGE")
[[ "$image_id" =~ ^sha256:[0-9a-f]{64}$ ]]
version="ghcr.io/athiwatc/formless:$GITHUB_REF_NAME"
latest=ghcr.io/athiwatc/formless:latest
tagged=()
cleanup() {
    if (( ${#tagged[@]} )); then docker image rm "${tagged[@]}"; fi
}
trap cleanup EXIT
for tag in "$version" "$latest"; do
    docker tag "$image_id" "$tag"
    tagged+=("$tag")
    test "$(docker image inspect --format '{{.Id}}' "$tag")" = "$image_id"
done
for tag in "${tagged[@]}"; do docker push "$tag"; done
