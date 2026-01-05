#!/bin/bash
# Fuzzing helper script for asset-io

set -e

echo "=== asset-io Fuzzing Helper ==="
echo ""

# Check for nightly toolchain
if ! rustup toolchain list | grep -q nightly; then
    echo "⚠️  Nightly toolchain not found. Installing..."
    rustup toolchain install nightly
fi

# Function to run a fuzz target
run_fuzz() {
    local target=$1
    local duration=${2:-60}  # Default 60 seconds
    
    echo "Running $target for ${duration} seconds..."
    cargo +nightly fuzz run $target -- -max_total_time=$duration
}

# Display available commands
case "${1:-help}" in
    list)
        echo "Available fuzz targets:"
        echo "  - fuzz_parse   : General file parsing (all formats)"
        echo "  - fuzz_write   : Write operations with updates"
        echo "  - fuzz_xmp     : XMP metadata parsing and modification"
        ;;
    
    parse)
        run_fuzz fuzz_parse ${2:-60}
        ;;
    
    write)
        run_fuzz fuzz_write ${2:-60}
        ;;
    
    xmp)
        run_fuzz fuzz_xmp ${2:-60}
        ;;
    
    all)
        echo "Running all fuzz targets for ${2:-60} seconds each..."
        run_fuzz fuzz_parse ${2:-60}
        run_fuzz fuzz_write ${2:-60}
        run_fuzz fuzz_xmp ${2:-60}
        ;;
    
    build)
        echo "Building all fuzz targets..."
        cargo +nightly fuzz build fuzz_parse
        cargo +nightly fuzz build fuzz_write
        cargo +nightly fuzz build fuzz_xmp
        echo "✅ All fuzz targets built successfully"
        ;;
    
    clean)
        echo "Cleaning fuzz artifacts..."
        rm -rf fuzz/artifacts/
        echo "✅ Cleaned"
        ;;
    
    *)
        echo "Usage: $0 <command> [duration_seconds]"
        echo ""
        echo "Commands:"
        echo "  list      - List available fuzz targets"
        echo "  parse     - Fuzz file parsing (default: 60s)"
        echo "  write     - Fuzz write operations (default: 60s)"
        echo "  xmp       - Fuzz XMP handling (default: 60s)"
        echo "  all       - Run all fuzz targets (default: 60s each)"
        echo "  build     - Build all fuzz targets"
        echo "  clean     - Remove fuzz artifacts"
        echo ""
        echo "Examples:"
        echo "  $0 parse 300       # Fuzz parsing for 5 minutes"
        echo "  $0 all 120         # Run all fuzzers for 2 minutes each"
        echo "  $0 build           # Just build without running"
        ;;
esac
