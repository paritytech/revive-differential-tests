#!/bin/bash

# Revive Differential Tests - Quick Start Script
# This script clones the test repository, sets up the corpus file, and runs the tool

set -e  # Exit on any error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
TEST_REPO_URL="https://github.com/paritytech/resolc-compiler-tests"
TEST_REPO_DIR="resolc-compiler-tests"
CORPUS_FILE="corpus.json"
WORKDIR="workdir"
NUMBER_OF_NODES=5

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

# Create corpus file with absolute path resolved at runtime
echo -e "${GREEN}Creating corpus file...${NC}"
ABSOLUTE_PATH=$(realpath "$TEST_REPO_DIR/fixtures/solidity")
cat > "$CORPUS_FILE" << EOF
{
  "name": "MatterLabs Solidity Simple, Complex, and Semantic Tests",
  "path": "$ABSOLUTE_PATH"
}
EOF

echo -e "${GREEN}Corpus file created: $CORPUS_FILE${NC}"

# Create workdir if it doesn't exist
mkdir -p "$WORKDIR"

echo -e "${GREEN}Starting differential tests...${NC}"
echo "This may take a while. Logs will be saved to logs.log and output.log"
echo ""

# Run the tool
RUST_LOG="info" cargo run --release -- \
    --corpus "$CORPUS_FILE" \
    --workdir "$WORKDIR" \
    --number-of-nodes "$NUMBER_OF_NODES" \
    > logs.log \
    2> output.log

echo -e "${GREEN}=== Test run completed! ===${NC}"
echo -e "Check ${GREEN}output.log${NC} for test results"
echo -e "Check ${GREEN}logs.log${NC} for detailed execution logs"