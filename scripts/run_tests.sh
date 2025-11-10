#!/bin/bash

# Revive Differential Tests - Quick Start Script
# This script clones the test repository, and runs the tool

set -e  # Exit on any error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
TEST_REPO_URL="https://github.com/paritytech/resolc-compiler-tests"
TEST_REPO_DIR="resolc-compiler-tests"
WORKDIR="workdir"

# Optional positional argument: path to polkadot-sdk directory
POLKADOT_SDK_DIR="${1:-}"

# Binary paths (default to names in $PATH)
REVIVE_DEV_NODE_BIN="revive-dev-node"
ETH_RPC_BIN="eth-rpc"

echo -e "${GREEN}=== Revive Differential Tests Quick Start ===${NC}"
echo ""

# Check if test repo already exists
if [ -d "$TEST_REPO_DIR" ]; then
    echo -e "${YELLOW}Test repository already exists. Pulling latest changes...${NC}"
    cd "$TEST_REPO_DIR"
    git pull
    cd ..
else
    echo -e "${GREEN}Cloning test repository...${NC}"
    git clone "$TEST_REPO_URL"
fi

# If polkadot-sdk path is provided, verify and use binaries from there; build if needed
if [ -n "$POLKADOT_SDK_DIR" ]; then
    if [ ! -d "$POLKADOT_SDK_DIR" ]; then
        echo -e "${RED}Provided polkadot-sdk directory does not exist: $POLKADOT_SDK_DIR${NC}"
        exit 1
    fi

    POLKADOT_SDK_DIR=$(realpath "$POLKADOT_SDK_DIR")
    echo -e "${GREEN}Using polkadot-sdk at: $POLKADOT_SDK_DIR${NC}"

    REVIVE_DEV_NODE_BIN="$POLKADOT_SDK_DIR/target/release/revive-dev-node"
    ETH_RPC_BIN="$POLKADOT_SDK_DIR/target/release/eth-rpc"

    if [ ! -x "$REVIVE_DEV_NODE_BIN" ] || [ ! -x "$ETH_RPC_BIN" ]; then
        echo -e "${YELLOW}Required binaries not found in release target. Building...${NC}"
        (cd "$POLKADOT_SDK_DIR" && cargo build --release --package staging-node-cli --package pallet-revive-eth-rpc --package revive-dev-node)
    fi

    for bin in "$REVIVE_DEV_NODE_BIN" "$ETH_RPC_BIN"; do
        if [ ! -x "$bin" ]; then
            echo -e "${RED}Expected binary not found after build: $bin${NC}"
            exit 1
        fi
    done
else
    echo -e "${YELLOW}No polkadot-sdk path provided. Using binaries from $PATH.${NC}"
fi

# Create workdir if it doesn't exist
mkdir -p "$WORKDIR"

echo -e "${GREEN}Starting differential tests...${NC}"
echo "This may take a while..."
echo ""

# Run the tool
cargo build --release;
RUST_LOG="info,alloy_pubsub::service=error" ./target/release/retester test \
    --platform revive-dev-node-polkavm-resolc \
    --test $(realpath "$TEST_REPO_DIR/fixtures/solidity") \
    --working-directory "$WORKDIR" \
    --concurrency.number-of-nodes 10 \
    --concurrency.number-of-threads 5 \
    --concurrency.number-of-concurrent-tasks 500 \
    --wallet.additional-keys 100000 \
    --revive-dev-node.path "$REVIVE_DEV_NODE_BIN" \
    --eth-rpc.path "$ETH_RPC_BIN" \
    > logs.log \
    2> output.log 

echo -e "${GREEN}=== Test run completed! ===${NC}"