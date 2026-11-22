PY      ?= .venv/bin/python
IMAGE   ?= shopping-sessions:latest
CPUS    ?= 8
MEM     ?= 24g
PLATFORM ?= linux/amd64
DEV     ?= data/dev
DATA    ?= data/dev
SEEDS   ?= 2-30
RUNS    ?= 3
CPUSET  ?= 0-3,8-11
BENCH_OUT ?= out-bench
# The command under test; override with CAND="target/release/shoptimizer" once the Rust pipeline exists.
CAND    ?= $(PY) reference/reference.py
# `bench`/`oracle` need the built binary, not the reference, unless CAND is given explicitly.
BENCH_CAND = $(if $(filter command line environment,$(origin CAND)),$(CAND),target/release/shoptimizer)
export IMAGE CPUS MEM PLATFORM

.PHONY: setup generator dev fixtures check-dev diff-seeds build docker-fixtures run-dev oracle bench

## One-time: Python venv with polars (for the test scripts) and the generator
setup:
	uv venv -q .venv && uv pip install -q --python $(PY) polars
	scripts/build_generator.sh
	unzip -oq dev-answer.zip -d .

## The public 1M-event dev set
dev:
	.tools/generate --seed 1 --events 1000000 --clicks 250000 --output $(DEV)

## Run CAND natively on all 36 fixtures
fixtures:
	$(PY) scripts/run_fixtures.py fixtures -- $(CAND)

## Run CAND natively on the dev set and diff against dev-answer/
check-dev:
	rm -rf out && time $(CAND) $(DEV) out && $(PY) scripts/compare.py out dev-answer

## Diff CAND against the reference on many generator seeds
diff-seeds:
	$(PY) scripts/diff_seeds.py --seeds $(SEEDS) -- $(CAND)

## Docker: build the scorer image, run it on the fixtures and dev set with scorer flags
build:
	docker buildx build --platform $(PLATFORM) --load -t $(IMAGE) .

docker-fixtures: build
	$(PY) scripts/run_fixtures.py fixtures -- scripts/docker_run.sh

run-dev: build
	rm -rf out && time scripts/docker_run.sh $(DEV) out && $(PY) scripts/compare.py out dev-answer

## Cache V1's output as the G4 oracle at $(DATA) (skips if $(DATA).v1out is complete)
oracle:
	PY="$(PY)" DATA="$(DATA)" CAND="$(BENCH_CAND)" scripts/bench.sh oracle

## Bench CAND RUNS times on $(DATA), print median wall + peak RSS, then diff against the oracle
bench:
	PY="$(PY)" DATA="$(DATA)" RUNS="$(RUNS)" CAND="$(BENCH_CAND)" OUT="$(BENCH_OUT)" CPUSET="$(CPUSET)" scripts/bench.sh bench
