# Developer shortcuts. `make` lists the targets.

PG_URL    ?= postgres:@127.0.0.1:55432/postgres
MYSQL_URL ?= root:@127.0.0.1:53306/app
SSH_URL   ?= tangul:tangul@127.0.0.1:52222
DEV_CONFIG := target/dev-config

# Local settings (see .env.example), e.g. the certificate that signs local builds.
-include .env
export TANGUL_SIGN_IDENTITY

.DEFAULT_GOAL := help
.PHONY: help setup tools docker-up db-up db-down db-reset db-logs psql mysql run run-release run-dev demo \
        test test-live test-live-native fmt lint license license-check ci app dmg install signing-cert release

help: ## List targets
	@grep -hE '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

setup: ## Check required tools
	@command -v rustup >/dev/null || { echo "missing: rustup (https://rustup.rs)"; exit 1; }
	@command -v docker >/dev/null || { echo "missing: docker (needed for dev databases)"; exit 1; }
	@if [ "$$(uname)" = "Darwin" ] && ! xcrun -f metal >/dev/null 2>&1; then \
		echo "missing: Metal toolchain (GPUI compiles its shaders with it)"; \
		echo "  run: xcodebuild -downloadComponent MetalToolchain"; exit 1; fi
	@rustup show active-toolchain >/dev/null && echo "ok"

docker-up: # Start Docker Desktop on macOS if the daemon isn't running (not listed in help)
	@docker info >/dev/null 2>&1 || { \
		if [ "$$(uname)" = "Darwin" ]; then \
			echo "starting Docker Desktop..."; open -ga Docker; \
			for i in $$(seq 1 60); do docker info >/dev/null 2>&1 && exit 0; sleep 2; done; \
			echo "Docker did not become ready in 2 minutes"; exit 1; \
		else echo "docker daemon is not running"; exit 1; fi; }

db-up: docker-up ## Start dev Postgres (:55432), MySQL (:53306) and an SSH bastion (:52222)
	docker compose up -d --wait
	@docker compose port ssh 22 | grep -q ':52222$$' || { \
		echo "The dev ports are taken by another compose project (docker compose ls); stop it first." >&2; \
		exit 1; }

db-down: ## Stop dev databases (data is kept)
	docker compose down

db-reset: ## Drop dev databases and re-seed from dev/seed/
	docker compose down -v
	docker compose up -d --wait

db-logs: ## Follow dev database logs
	docker compose logs -f

psql: ## Shell into dev Postgres
	docker compose exec postgres psql -U postgres

mysql: ## Shell into dev MySQL
	docker compose exec mysql mysql -uroot --default-character-set=utf8mb4 app

run: ## Run the app (debug) with your normal profiles
	cargo run

run-release: ## Run the app (release) with your normal profiles
	cargo run --release

demo: db-up ## Record docs/demo.webp, the README animation (macOS)
	./scripts/demo.sh

run-dev: db-up ## Start dev databases and run the app with dev profiles only
	@mkdir -p $(DEV_CONFIG)
	@[ -f $(DEV_CONFIG)/profiles.toml ] || cp dev/profiles.toml $(DEV_CONFIG)/profiles.toml
	TANGUL_CONFIG_DIR=$(DEV_CONFIG) cargo run

test: ## Unit tests (no databases needed)
	cargo test --workspace

test-live: db-up ## Integration tests against the dev databases
	TANGUL_TEST_PG=$(PG_URL) TANGUL_TEST_MYSQL=$(MYSQL_URL) TANGUL_TEST_SSH=$(SSH_URL) \
		cargo test -p tangul-engine

test-live-native: ## test-live without Docker (Debian/Ubuntu, e.g. cloud sessions): databases via apt
	scripts/dev-db-native.sh
	TANGUL_TEST_PG=$(PG_URL) TANGUL_TEST_MYSQL=$(MYSQL_URL) TANGUL_TEST_SSH=$(SSH_URL) \
		TANGUL_TEST_SSH_DB=127.0.0.1:55432 cargo test -p tangul-engine

app: ## Build target/bundle/tangul.app and a DMG (UNIVERSAL=1 for Apple Silicon + Intel)
	scripts/bundle-macos.sh $(if $(UNIVERSAL),--universal)

dmg: app ## Same as app (the DMG is built alongside)

signing-cert: ## Create a local code-signing certificate, so installs keep Keychain access
	scripts/local-signing-cert.sh

release: ## Release main: asks for the version, bumps and tags it
	scripts/release.sh

install: app ## Build the app and copy it to /Applications
	rm -rf /Applications/tangul.app
	cp -R target/bundle/tangul.app /Applications/
	@echo "installed /Applications/tangul.app"

fmt: ## Format code
	cargo fmt --all

tools: ## Install the lint and license tools (cargo-deny, cargo-machete, cargo-about)
	cargo install --locked cargo-deny cargo-machete
	cargo install --locked cargo-about --features cli

lint: ## Format check, clippy (warnings are errors), cargo-deny, unused deps
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	@command -v cargo-deny >/dev/null || { echo "missing cargo-deny: make tools"; exit 1; }
	cargo deny check
	@command -v cargo-machete >/dev/null || { echo "missing cargo-machete: make tools"; exit 1; }
	cargo machete

license: ## Check licenses (own crates AGPL, deps allowed) and regenerate THIRD_PARTY_LICENSES.md
	@head -n 1 LICENSE | grep -q 'GNU AFFERO GENERAL PUBLIC LICENSE' || { echo "LICENSE is not the AGPL-3.0 text"; exit 1; }
	@cargo metadata --no-deps --format-version 1 | python3 -c 'import json, sys; \
		bad = [p["name"] for p in json.load(sys.stdin)["packages"] if p["license"] != "AGPL-3.0-only" or p["publish"] != []]; \
		sys.exit("not AGPL-3.0-only with publish = false: " + ", ".join(bad) if bad else 0)'
	@command -v cargo-deny >/dev/null || { echo "missing cargo-deny: make tools"; exit 1; }
	cargo deny check licenses
	@command -v cargo-about >/dev/null || { echo "missing cargo-about: make tools"; exit 1; }
	cargo about generate -c packaging/licenses/about.toml packaging/licenses/about.hbs \
		-o THIRD_PARTY_LICENSES.md

license-check: license ## Same as license, fails if THIRD_PARTY_LICENSES.md was out of date (CI)
	@git diff --quiet -- THIRD_PARTY_LICENSES.md && git ls-files --error-unmatch THIRD_PARTY_LICENSES.md >/dev/null 2>&1 || \
		{ echo "THIRD_PARTY_LICENSES.md is out of date: run make license and commit it"; exit 1; }

ci: lint license-check test test-live ## Everything CI runs
