#!/usr/bin/env bash
# `make demo`: records docs/demo.webp, the README animation, from the real app on
# the dev Postgres (see crates/app/src/workspace/demo.rs). Needs img2webp (brew
# install webp) and a Mac (Metal).
set -euo pipefail
cd "$(dirname "$0")/.."

command -v img2webp >/dev/null || { echo "missing: img2webp (brew install webp)"; exit 1; }
# The assistant's SELECT-only user, as the database form would set it up.
docker compose exec -T postgres psql -q -U postgres -v ON_ERROR_STOP=1 <<'SQL'
DO $$ BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'ai_reader') THEN
        CREATE ROLE ai_reader LOGIN;
    END IF;
END $$;
GRANT USAGE ON SCHEMA app TO ai_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA app TO ai_reader;
SQL

work=target/demo
rm -rf "$work"
mkdir -p "$work/config"
cp dev/demo/profiles.toml dev/demo/settings.json "$work/config/"

TANGUL_CONFIG_DIR="$work/config" TANGUL_DEMO="$work/frames" \
    cargo run --release --features demo

# Every frame at its own duration; lossy, but the text stays sharp at q 80.
args=(-loop 0 -lossy -q 80 -m 6)
while read -r ms frame; do
    args+=(-d "$ms" "$work/frames/$frame")
done < "$work/frames/frames.txt"
img2webp "${args[@]}" -o docs/demo.webp
ls -lh docs/demo.webp
