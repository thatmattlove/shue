SHELL := /bin/sh

CARGO ?= cargo
SHUE_PACKAGE ?= shue
SHUE_CORE_PACKAGE ?= shue-core
ARGS ?=

.DEFAULT_GOAL := help
.NOTPARALLEL: verify

.PHONY: help all build release check test fmt fmt-check clippy quality \
	acceptance performance msrv license-policy release-check release-pipeline-check interactive \
	verify install run clean

help: ## Show the available targets
	@awk 'BEGIN { FS = ":.*##[[:space:]]*"; print "shue development targets:" } \
		/^[[:alnum:]_-]+:.*##[[:space:]]*/ { printf "  %-16s %s\n", $$1, $$2 }' \
		$(MAKEFILE_LIST)

all: build ## Build the debug binary

build: ## Build shue with the development profile
	$(CARGO) build -p $(SHUE_PACKAGE)

release: ## Build the optimized release binary
	$(CARGO) build --release -p $(SHUE_PACKAGE)

check: ## Type-check every workspace target
	$(CARGO) check --workspace --all-targets

test: ## Run all workspace tests
	$(CARGO) test --workspace --all-targets -- --nocapture

fmt: ## Format the Rust workspace
	$(CARGO) fmt --all

fmt-check: ## Verify Rust formatting without changing files
	$(CARGO) fmt --all -- --check

clippy: ## Run Clippy across every target with warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

quality: ## Run the repository formatting and Clippy checks
	./scripts/check-quality.sh

acceptance: ## Run optimized CLI and core acceptance suites
	$(CARGO) test --release -p $(SHUE_PACKAGE) --test e2e --test modes -- --nocapture
	$(CARGO) test --release -p $(SHUE_CORE_PACKAGE) --test acceptance -- --nocapture

performance: ## Measure optimized highlighting throughput
	$(CARGO) test --release -p $(SHUE_CORE_PACKAGE) --test performance -- --nocapture

msrv: ## Test every target with the minimum supported Rust toolchain
	./scripts/check-msrv.sh

license-policy: ## Verify that project licensing is consistently MIT-only
	./scripts/check-license-policy.sh

release-check: ## Build and validate the release CLI surface
	./scripts/check-release.sh

release-pipeline-check: ## Validate release packaging, workflow, and documentation
	./scripts/check-release-pipeline.sh
	./scripts/check-release-docs.sh

interactive: ## Verify real-PTY behavior and terminal restoration
	./scripts/check-interactive.py

verify: test quality acceptance performance msrv license-policy release-check release-pipeline-check interactive ## Run the complete local verification suite

install: ## Install shue from this checkout using Cargo
	$(CARGO) install --path crates/shue-cli --locked

run: ## Run shue; pass arguments with ARGS='--help'
	$(CARGO) run -p $(SHUE_PACKAGE) -- $(ARGS)

clean: ## Remove Cargo build artifacts
	$(CARGO) clean
