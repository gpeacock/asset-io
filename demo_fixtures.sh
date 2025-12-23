#!/bin/bash
# Demo script showing the fixture system in action

set -e

echo "======================================"
echo "JUMBF-IO Test Fixture System Demo"
echo "======================================"
echo ""

echo "1️⃣  File-based fixtures (default mode)"
echo "   Loading from tests/fixtures/"
echo ""
cargo run --example test_fixtures 2>&1 | grep -A2 "Testing All Fixtures"
echo ""

echo "2️⃣  Embedded fixtures (fast CI mode)"
echo "   Fixtures compiled into binary"
echo ""
cargo run --example test_fixtures --features embed-fixtures 2>&1 | grep "Testing" | head -3
echo ""

echo "3️⃣  Extended fixtures (optional large test set)"
echo ""
if [ -z "$JUMBF_TEST_FIXTURES" ]; then
    echo "   ℹ️  JUMBF_TEST_FIXTURES not set"
    echo "   To use extended fixtures:"
    echo "   export JUMBF_TEST_FIXTURES=/path/to/extended/fixtures"
    echo "   cargo run --example test_fixtures"
else
    echo "   ✅ Using extended fixtures from: $JUMBF_TEST_FIXTURES"
    cargo run --example test_fixtures 2>&1 | grep "Found"
fi
echo ""

echo "4️⃣  Running all tests"
echo ""
cargo test 2>&1 | tail -5
echo ""

echo "======================================"
echo "✅ All demos complete!"
echo "======================================"
echo ""
echo "Next steps:"
echo "  • See TESTING.md for detailed documentation"
echo "  • Run 'cargo run --example test_fixtures' (test-utils enabled by default)"
echo "  • Try 'cargo test --features embed-fixtures' for fast CI"
echo "  • Set JUMBF_TEST_FIXTURES for extended testing"

