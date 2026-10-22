#!/usr/bin/env bash
# Releases main: asks for the version, bumps it in Cargo.toml on main, tags that
# commit and pushes both, then merges main into dev; the Release workflow builds the
# universal DMG and publishes the GitHub release.
#
#   make release
#
# Needs `gh` logged in; switching to main needs a clean working tree.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { echo "release: $*" >&2; exit 1; }
cargo_version() { sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1; }

# In a function, read whole before it runs: switching branches can change this file.
release() {
    command -v gh >/dev/null || die "needs the GitHub CLI (gh)"
    gh auth status >/dev/null 2>&1 || die "gh is not logged in: gh auth login"

    # On main, up to date.
    branch=$(git symbolic-ref --quiet --short HEAD || echo "(detached)")
    if [[ "$branch" != main ]]; then
        read -rp "Releases are made from main, you are on $branch. Switch to main? [y/N] " answer
        [[ "$answer" == [yY] ]] || die "stopped"
        [[ -z "$(git status --porcelain)" ]] || die "the working tree has changes; commit or stash them first"
        git switch --quiet main
    fi
    [[ -z "$(git status --porcelain)" ]] || die "main has uncommitted changes"
    git pull --quiet --ff-only origin main
    git fetch --quiet --tags origin

    # A version that is higher than the current one and not taken yet.
    current=$(cargo_version)
    [[ -n "$current" ]] || die "no version in Cargo.toml"
    while true; do
        read -rp "Version (current: $current): " version
        version="${version#v}"
        tag="v$version"
        if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            echo "  $version is not x.y.z"
        elif [[ "$version" == "$current" ||
            "$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -n 1)" != "$version" ]]; then
            echo "  has to be higher than the current $current"
        elif git rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
            [[ -n "$(git ls-remote --tags origin "refs/tags/$tag")" ]]; then
            echo "  tag $tag exists already"
        elif gh release view "$tag" >/dev/null 2>&1; then
            echo "  release $tag exists already"
        else
            break
        fi
    done

    # Before anything changes: the dev migration steps have to be this version's.
    scripts/release-migrations.py --check "$version"

    read -rp "Release main ($(git rev-parse --short HEAD)) as $tag? [y/N] " answer
    [[ "$answer" == [yY] ]] || die "stopped"

    # Only the workspace version: the first `version = ` line, in [workspace.package].
    sed -i.bak "1,/^version = /s/^version = \".*\"$/version = \"$version\"/" Cargo.toml
    rm Cargo.toml.bak
    [[ "$(cargo_version)" == "$version" ]] || die "Cargo.toml didn't take the new version"
    # Updates the workspace crates' versions in Cargo.lock.
    cargo metadata --format-version 1 >/dev/null
    # The settings files' dev migration steps become this release's one step, and
    # its layouts fixtures (crates/data/migrations/CLAUDE.md).
    scripts/release-migrations.py "$version"
    git add crates/data/migrations
    git commit --quiet -m "Release $tag" -- Cargo.toml Cargo.lock crates/data/migrations
    git tag -a "$tag" -m "tangul $tag"
    # Both or neither: the tag's build needs the commit.
    git push --quiet --atomic origin main "$tag"
    # dev gets the new version (and the joined migration steps) too.
    git switch --quiet dev
    git pull --quiet --ff-only origin dev
    git merge --quiet --no-edit main || die "merging main into dev failed: resolve it on dev and push"
    git push --quiet origin dev
    git switch --quiet main
    echo "released $tag: the Release workflow builds and publishes it"
    echo "  gh run watch \$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
}

release
