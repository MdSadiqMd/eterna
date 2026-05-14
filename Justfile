set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Build images and start engine + 3 api replicas behind nginx on :80
docker-up:
    docker compose up --build --scale api=3

# Stop and remove all containers and networks
docker-down:
    docker compose down

_pids := ".local.pids"

# Build and start engine (:9000) + api (:8080) in the background
local-up:
    #!/usr/bin/env bash
    if [ -f {{_pids}} ]; then
        echo "already running — stop with: just local-down"
        exit 1
    fi
    cargo build --bin engine --bin api
    ./target/debug/engine &
    echo $! > {{_pids}}
    sleep 0.3
    ./target/debug/api &
    echo $! >> {{_pids}}
    echo "engine :9000  api :8080  (pids: $(tr '\n' ' ' < {{_pids}}))"
    echo "stop with: just local-down"

# Kill the background engine + api processes
local-down:
    #!/usr/bin/env bash
    if [ ! -f {{_pids}} ]; then
        echo "nothing running"
        exit 0
    fi
    while IFS= read -r pid; do
        kill "$pid" 2>/dev/null && echo "killed $pid" || echo "$pid already gone"
    done < {{_pids}}
    rm -f {{_pids}}

# Run all unit and integration tests across the workspace
test:
    cargo test --tests

# Run the end-to-end demo
test_api:
    cargo run --release --bin test_api

# Run the concurrent stress test (16 instances × 5000 orders, all invariants checked)
stress:
    cargo run --release --bin stress

# Generate a flamegraph for the bench workload (auto-increments: 01.svg, 02.svg, …)
flamegraph:
    ./flamegraph.sh
