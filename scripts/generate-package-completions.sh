#!/bin/sh
# Generate from the just-built binary before either Linux package is assembled.
set -eu
if [ "$#" -ne 2 ]; then
    echo "usage: $0 BINARY OUTPUT_DIRECTORY" >&2
    exit 2
fi
binary=$1
output=$2
mkdir -p "$output"
stage=$(mktemp -d "$output/.generate.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT HUP INT TERM
for shell in bash zsh fish; do
    if ! "$binary" completions "$shell" > "$stage/$shell"; then
        echo "failed to generate $shell completion" >&2
        exit 1
    fi
    if [ ! -s "$stage/$shell" ]; then
        echo "empty $shell completion" >&2
        exit 1
    fi
done
# Install only after all three generators have succeeded with nonempty output.
install -m 644 "$stage/bash" "$output/formless.bash"
install -m 644 "$stage/zsh" "$output/_formless"
install -m 644 "$stage/fish" "$output/formless.fish"
