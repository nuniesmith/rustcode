#!/usr/bin/env bash
# release.sh — cut a tagged release of rustcode.
#
# rustcode is distributed with `cargo install --git` (see README), NOT through
# crates.io: the crate name `rustcode` and every internal crate name (`api`,
# `runtime`, `tools`, `plugins`, `rag`, `telemetry`, `commands`) are already
# taken on the registry. A "release" here is therefore just: bump the workspace
# version, validate the build, commit, tag, and push. Users then install a
# specific release with:
#
#   cargo install --git https://github.com/nuniesmith/rustcode --tag vX.Y.Z --locked rustcode
#
# Usage:
#   ./scripts/release.sh                 # tag the current Cargo.toml version
#   ./scripts/release.sh <x.y.z>         # set an explicit version, then tag
#   ./scripts/release.sh patch|minor|major
#   add --dry-run to rehearse without committing, tagging, or pushing.
#
# The version is read from [workspace.package].version in Cargo.toml — never
# from git tags (a missing/stale tag must not silently downgrade the project).
# A downgrade aborts. Every step is idempotent, so a run that fails partway can
# be re-run safely.
set -euo pipefail

# ── Repo root (works regardless of CWD) ───────────────────────────────────────
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
CARGO="Cargo.toml"
REPO_URL="https://github.com/nuniesmith/rustcode"

# ── Args ──────────────────────────────────────────────────────────────────────
DRY_RUN=false
BUMP_ARG=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
        *) BUMP_ARG="$arg" ;;
    esac
done
$DRY_RUN && echo "==> DRY RUN — no commits, tags, or pushes"

run() { if $DRY_RUN; then echo "[dry-run] $*"; else "$@"; fi; }

# ── Current version (from [workspace.package], not from tags) ─────────────────
CURRENT=$(grep -A20 '^\[workspace.package\]' "$CARGO" \
    | grep -E '^\s*version\s*=' | head -n1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
if [[ -z "$CURRENT" ]]; then
    echo "ERROR: could not read [workspace.package].version from $CARGO"
    exit 1
fi
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

# ── Resolve the target version (no arg = tag the current version as-is) ───────
if [[ -z "$BUMP_ARG" ]]; then
    NEW_VERSION="$CURRENT"
    echo "==> No version argument — releasing current version $CURRENT as-is"
else
    case "$BUMP_ARG" in
        major) NEW_VERSION="$((MAJOR + 1)).0.0" ;;
        minor) NEW_VERSION="${MAJOR}.$((MINOR + 1)).0" ;;
        patch) NEW_VERSION="${MAJOR}.${MINOR}.$((PATCH + 1))" ;;
        [0-9]*.[0-9]*.[0-9]*) NEW_VERSION="$BUMP_ARG" ;;
        *) echo "ERROR: invalid version/bump '$BUMP_ARG'"; exit 1 ;;
    esac
    echo "==> Version: $CURRENT -> $NEW_VERSION"
fi
NEW_TAG="v${NEW_VERSION}"

# ── Guard: never downgrade (sort -V puts the larger version last) ─────────────
if [[ "$NEW_VERSION" != "$CURRENT" ]]; then
    LOWER=$(printf '%s\n%s\n' "$CURRENT" "$NEW_VERSION" | sort -V | head -n1)
    if [[ "$LOWER" == "$NEW_VERSION" ]]; then
        echo "ERROR: $NEW_VERSION is older than current $CURRENT — refusing to downgrade."
        exit 1
    fi
fi

# ── Working tree must be clean unless we're only re-tagging the current
#    version (no Cargo.toml edit needed in that case). ──────────────────────────
if [[ "$NEW_VERSION" != "$CURRENT" && -n "$(git status --porcelain)" ]]; then
    echo "ERROR: working tree is dirty — commit or stash changes first"
    exit 1
fi

# ── Rewrite Cargo.toml: bump the version in BOTH [package] and
#    [workspace.package]. Member crates inherit it via `version.workspace = true`.
if [[ "$NEW_VERSION" != "$CURRENT" ]] && ! $DRY_RUN; then
    NEW_VERSION="$NEW_VERSION" python3 - "$CARGO" <<'PY'
import os, re, sys
path = sys.argv[1]
new = os.environ["NEW_VERSION"]
section = None
out = []
for line in open(path).read().splitlines(keepends=True):
    s = line.strip()
    if s.startswith("[") and s.endswith("]"):
        section = s[1:-1]
    if section in ("package", "workspace.package") and re.match(r'\s*version\s*=', line):
        line = re.sub(r'(version\s*=\s*)"[^"]*"', rf'\g<1>"{new}"', line)
    out.append(line)
open(path, "w").write("".join(out))
PY
    echo "==> Updated $CARGO to $NEW_VERSION"
    grep -nE '^\s*version\s*=' "$CARGO" | grep "$NEW_VERSION" | head
elif [[ "$NEW_VERSION" != "$CURRENT" ]]; then
    echo "[dry-run] would set [package] + [workspace.package] version to $NEW_VERSION"
fi

# ── Sanity: the whole workspace still resolves + builds at the version ────────
echo "==> cargo check --workspace"
run cargo check --workspace --all-features

# ── Commit (only if the version actually changed) ─────────────────────────────
if [[ "$NEW_VERSION" != "$CURRENT" ]]; then
    run git add "$CARGO"
    if $DRY_RUN; then
        echo "[dry-run] git commit -m \"chore: release ${NEW_VERSION}\""
    elif git diff --cached --quiet; then
        echo "==> nothing staged to commit"
    else
        git commit -m "chore: release ${NEW_VERSION}"
    fi
fi

# ── Tag (idempotent) ──────────────────────────────────────────────────────────
if $DRY_RUN; then
    echo "[dry-run] git tag -a $NEW_TAG -m \"Release ${NEW_TAG}\""
elif git rev-parse "$NEW_TAG" >/dev/null 2>&1; then
    echo "==> Tag $NEW_TAG already exists — reusing it"
else
    git tag -a "$NEW_TAG" -m "Release ${NEW_TAG}"
fi

# ── Push branch + tag (idempotent; a no-op if already up to date) ─────────────
BRANCH=$(git rev-parse --abbrev-ref HEAD)
echo "==> Pushing '$BRANCH' and tag '$NEW_TAG'"
run git push origin "$BRANCH"
run git push origin "$NEW_TAG"

echo ""
if $DRY_RUN; then
    echo "✓ Dry run complete — ${NEW_TAG} would be released."
else
    echo "✓ Released ${NEW_TAG}."
    echo ""
    echo "  GitHub Actions (.github/workflows/release.yml) is now building prebuilt"
    echo "  binaries and will attach them to:"
    echo "    ${REPO_URL}/releases/tag/${NEW_TAG}"
    echo ""
    echo "  Install from source in the meantime with:"
    echo "    cargo install --git ${REPO_URL} --tag ${NEW_TAG} --locked rustcode"
    echo "    cargo install --git ${REPO_URL} --tag ${NEW_TAG} --locked rusty-claude-cli"
fi
