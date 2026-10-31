# Build stages share generated outputs; serialize prerequisites even under make -j.
.NOTPARALLEL:
.DEFAULT_GOAL := build
export CARGO_BUILD_JOBS := 4
E2E_DEVICE ?= emulator-5580
DEVICE ?=
export E2E_DEVICE DEVICE
.PHONY: bindings rust build test e2e-emulator deploy
bindings:
	python3 tools/android.py bindings
rust:
	python3 tools/android.py rust
build: bindings rust
	python3 tools/android.py assemble
test: bindings
	python3 tools/android.py test
e2e-emulator:
	python3 tools/android.py e2e
deploy:
	python3 tools/android.py deploy
