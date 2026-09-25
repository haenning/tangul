# Developer shortcuts. `make` lists the targets.

PG_URL    ?= postgres:@127.0.0.1:55432/postgres
MYSQL_URL ?= root:@127.0.0.1:53306/app
SSH_URL   ?= tangul:tangul@127.0.0.1:52222
DEV_CONFIG := target/dev-config

.DEFAULT_GOAL := help
.PHONY: help setup tools docker-up db-up db-down db-reset db-logs psql mysql run run-release run-dev \
        test test-live fmt lint ci app dmg install

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

run-dev: db-up ## Start dev databases and run the app with dev profiles only
	@mkdir -p $(DEV_CONFIG)
	@[ -f $(DEV_CONFIG)/profiles.toml ] || cp dev/profiles.toml $(DEV_CONFIG)/profiles.toml
	TANGUL_CONFIG_DIR=$(DEV_CONFIG) cargo run

test: ## Unit tests (no databases needed)
	cargo test --workspace

test-live: db-up ## Integration tests against the dev databases
	TANGUL_TEST_PG=$(PG_URL) TANGUL_TEST_MYSQL=$(MYSQL_URL) TANGUL_TEST_SSH=$(SSH_URL) \
		cargo test -p tangul-engine

app: ## Build target/bundle/tangul.app and a DMG (UNIVERSAL=1 for Apple Silicon + Intel)
	scripts/bundle-macos.sh $(if $(UNIVERSAL),--universal)

dmg: app ## Same as app (the DMG is built alongside)

install: app ## Build the app and copy it to /Applications
	rm -rf /Applications/tangul.app
	cp -R target/bundle/tangul.app /Applications/
	@echo "installed /Applications/tangul.app"

fmt: ## Format code
	cargo fmt --all

tools: ## Install the lint tools (cargo-deny, cargo-machete)
	cargo install --locked cargo-deny cargo-machete

lint: ## Format check, clippy (warnings are errors), cargo-deny, unused deps
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	@command -v cargo-deny >/dev/null || { echo "missing cargo-deny: make tools"; exit 1; }
	cargo deny check
	@command -v cargo-machete >/dev/null || { echo "missing cargo-machete: make tools"; exit 1; }
	cargo machete

ci: lint test test-live ## Everything CI runs
