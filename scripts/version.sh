#!/bin/sh
# Prints the version of vericount (from Cargo.toml), or, with an argument
# (major, minor or patch), sets the next one in every file that holds it.
set -eu
cd "$(dirname "$0")/.."

current=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[ -n "$current" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
[ $# -eq 0 ] && { echo "$current"; exit 0; }

IFS=. read -r major minor patch <<EOF2
$current
EOF2
case "$1" in
    major) next="$((major + 1)).0.0" ;;
    minor) next="$major.$((minor + 1)).0" ;;
    patch) next="$major.$minor.$((patch + 1))" ;;
    *) echo "usage: $0 [major|minor|patch]" >&2; exit 2 ;;
esac

sed -i "/^\[workspace.package\]/,/^\[/s/^version = \".*\"/version = \"$next\"/" Cargo.toml
# The workspace crates in the lock file.
for crate in vericount-core vericount-server vericount-aeat-mock; do
    sed -i "/^name = \"$crate\"$/{n;s/^version = \".*\"/version = \"$next\"/}" Cargo.lock
done
sed -i "s/^version: .*/version: $next/; s/^appVersion: .*/appVersion: \"$next\"/" charts/vericount/Chart.yaml
echo "$next"
