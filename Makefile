PY      ?= .venv/bin/python
IMAGE   ?= shopping-sessions:latest
CPUS    ?= 8
MEM     ?= 24g
PLATFORM ?= linux/amd64
DEV     ?= data/dev
DATA    ?= data/dev
SEEDS   ?= 2-30
RUNS    ?= 3
# The command under test; override with CAND="target/release/shoptimizer" once the Rust pipeline exists.
CAND    ?= $(PY) reference/reference.py
# `bench` defaults to the built binary, but honours an explicit CAND. `oracle` always uses
# the fixed V1 binary that scripts/bench.sh hard-codes, so CAND cannot change the oracle.
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
	DATA="$(DATA)" scripts/bench.sh oracle

## Bench CAND RUNS times on $(DATA) in out-bench/, print median wall + peak RSS, then diff
bench:
	PY="$(PY)" DATA="$(DATA)" RUNS="$(RUNS)" CAND="$(BENCH_CAND)" scripts/bench.sh bench
