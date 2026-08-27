#!/bin/sh
set -eu

cargo build --release -p shue

binary="target/release/shue"
test -x "$binary"
help="$($binary --help)"
version="$($binary --version)"

for required in "username@host" "--ssh-path" "SHUE_SSH" "--exec" "--filter" "--color-depth"; do
    case "$help" in
        *"$required"*) ;;
        *)
            echo "release help is missing required text: $required" >&2
            exit 1
            ;;
    esac
done

case "$version" in
    shue*) ;;
    *)
        echo "unexpected version output: $version" >&2
        exit 1
        ;;
esac

echo "release verification passed"

