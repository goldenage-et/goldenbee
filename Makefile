.NOTPARALLEL:
.PHONY: setup check check-ci build build\:embedded web format format-check lint test test-fixtures docs dev dev-status dev-stop start stop worker\:general worker\:general\:build worker\:general\:dev worker\:general\:run worker\:general\:stop

# `make start gateway`, `make start gb-client`, `make start worker general`
START_ARGS := $(filter-out start,$(MAKECMDGOALS))
STOP_ARGS := $(filter-out stop,$(MAKECMDGOALS))
ifneq (,$(filter start,$(MAKECMDGOALS)))
.PHONY: $(START_ARGS)
$(START_ARGS):
	@:
endif
ifneq (,$(filter stop,$(MAKECMDGOALS)))
.PHONY: $(STOP_ARGS)
$(STOP_ARGS):
	@:
endif

CONFIG ?= config/general.toml
WORKER_ADDR ?= 127.0.0.1:8080
WORKER_AGENT ?= general

check: format-check lint test docs

# Hosted runners cannot reliably provide the ports, processes, browsers, and
# timing that the integration suite needs. Keep CI deterministic; run the full
# `make check` locally before a release.
check-ci: format-check lint test-fixtures
	cargo fetch --locked
	RUST_MIN_STACK=16777216 RUST_TEST_THREADS=1 cargo test --workspace --all-features --lib --locked
	python3 -m unittest discover -s tests/smoke -v

build:
	cargo build --workspace --locked

# Release Gateway binaries embed the built browser Client. Debug builds read
# apps/goldenbee-web/dist from disk, so Rust-only iteration does not need Bun.
web:
	bun install --frozen-lockfile
	bun run build

build\:embedded: web
	cargo build --release --locked -p goldenbee-gateway

format:
	cargo fmt --all

format-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

test: test-fixtures
	RUST_MIN_STACK=16777216 RUST_TEST_THREADS=1 cargo test --workspace --all-features --locked
	python3 -m unittest discover -s tests/smoke -v

test-fixtures:
	cargo build --workspace --bins --locked

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked

setup:
	@scripts/dev.sh setup

dev:
	@scripts/dev.sh start all
	@scripts/dev.sh start gb-client

dev-status:
	@scripts/dev.sh status

dev-stop:
	@scripts/dev.sh stop all

start:
	@scripts/dev.sh start $(START_ARGS)

stop:
	@scripts/dev.sh stop $(STOP_ARGS)

worker\:general\:build:
	cargo build --locked -p general

worker\:general: worker\:general\:build
	@set -a; if [ -f .env ]; then . ./.env; fi; set +a; \
	GOLDENBEE_DATA_DIR="$(CURDIR)/general/state" GOLDENBEE_DEV_UI=1 ./target/debug/general \
		--config "$(CONFIG)" serve --addr "$(WORKER_ADDR)"

worker\:general\:dev: worker\:general\:build
	@set -a; if [ -f .env ]; then . ./.env; fi; set +a; \
	./target/debug/general --config "$(CONFIG)" dev --agent "$(WORKER_AGENT)"

worker\:general\:run: worker\:general\:build
	@set -a; if [ -f .env ]; then . ./.env; fi; set +a; \
	./target/debug/general --config "$(CONFIG)" run \
		--agent "$(WORKER_AGENT)" --input "$(INPUT)" --json

worker\:general\:stop:
	-@pgrep -x general | xargs -r kill
