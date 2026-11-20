#!/bin/sh
# Builds the dataset generator from generator.zip into .tools/generate (needs Rust 1.85+).
set -eu
cd "$(dirname "$0")/.."
rm -rf .tools/generator && mkdir -p .tools
unzip -q generator.zip -d .tools
(cd .tools/generator && cargo build --release --quiet)
cp .tools/generator/target/release/generate .tools/generate
echo "built .tools/generate"
