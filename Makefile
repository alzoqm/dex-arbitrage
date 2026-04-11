SHELL := /bin/bash

run-base:
	cargo run --release -- --chain base

run-base-sim:
	cargo run --release -- --chain base --simulate-only

run-polygon:
	cargo run --release -- --chain polygon

run-polygon-sim:
	cargo run --release -- --chain polygon --simulate-only

build-rust:
	cargo build --release

test-rust:
	cargo test --all -- --nocapture

build-contracts:
	forge build

deploy-base:
	./scripts/deploy_base.sh

deploy-polygon:
	./scripts/deploy_polygon.sh
