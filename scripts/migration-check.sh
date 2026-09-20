#!/bin/sh
set -eu

map=e2e-tests/MIGRATION.md
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/amux-migration-check.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

awk -F '|' '
function trim(value) {
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    return value
}
/^\| `e2e-tests\// {
    file = trim($2)
    assertion = trim($3)
    disposition = trim($4)
    replacement = trim($5)
    status = trim($6)
    gsub(/^`|`$/, "", file)
    print file "\t" assertion "\t" disposition "\t" replacement "\t" status
}
' "$map" >"$tmp_dir/rows"

if [ ! -s "$tmp_dir/rows" ]; then
    echo "migration map has no file rows: $map" >&2
    exit 1
fi

cut -f1 "$tmp_dir/rows" | sort | uniq -d >"$tmp_dir/duplicates"
if [ -s "$tmp_dir/duplicates" ]; then
    echo "migration map lists files more than once:" >&2
    sed 's/^/  /' "$tmp_dir/duplicates" >&2
    exit 1
fi

tab=$(printf '\t')
while IFS="$tab" read -r file assertion disposition replacement status; do
    if [ -z "$assertion" ] || [ -z "$replacement" ]; then
        echo "migration row is missing its assertion or replacement: $file" >&2
        exit 1
    fi
    case "$disposition" in
        retain|move|replace|delete) ;;
        *)
            echo "invalid disposition '$disposition' for $file" >&2
            exit 1
            ;;
    esac
    case "$status" in
        pending)
            if [ ! -f "$file" ]; then
                echo "pending migration source does not exist: $file" >&2
                exit 1
            fi
            ;;
        done) ;;
        *)
            echo "invalid status '$status' for $file" >&2
            exit 1
            ;;
    esac
done <"$tmp_dir/rows"

find e2e-tests -maxdepth 1 -type f ! -name MIGRATION.md -print | sort >"$tmp_dir/actual"
cut -f1 "$tmp_dir/rows" | sort -u >"$tmp_dir/listed"
comm -23 "$tmp_dir/actual" "$tmp_dir/listed" >"$tmp_dir/unlisted"
if [ -s "$tmp_dir/unlisted" ]; then
    echo "top-level e2e files are missing from the migration map:" >&2
    sed 's/^/  /' "$tmp_dir/unlisted" >&2
    exit 1
fi

echo "migration map accounts for $(wc -l <"$tmp_dir/rows" | tr -d ' ') files"
