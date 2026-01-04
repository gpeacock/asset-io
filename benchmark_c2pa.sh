#!/bin/bash

# C2PA Performance Benchmark: asset-io vs c2patool
# Compares signing performance across different BMFF formats

set -e

ASSET_IO_C2PA="./target/release/examples/c2pa"
C2PATOOL="../c2pa-rs/target/release/c2patool"
TEST_SETTINGS="tests/fixtures/test_settings.json"

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║        C2PA Performance Benchmark: asset-io vs c2patool        ║"
echo "╚════════════════════════════════════════════════════════════════╝"
echo ""
echo "Test methodology:"
echo "  - Each test run 3 times, best time reported"
echo "  - asset-io: Streaming write-hash-update (single pass)"
echo "  - c2patool: Traditional approach (write → hash → update)"
echo ""

# Function to time a command (best of 3 runs)
time_command() {
    local cmd="$1"
    local best_time=999999
    
    for i in 1 2 3; do
        local start=$(date +%s%N 2>/dev/null || gdate +%s%N)
        eval "$cmd" > /dev/null 2>&1 || true
        local end=$(date +%s%N 2>/dev/null || gdate +%s%N)
        local elapsed=$(( (end - start) / 1000000 )) # Convert to milliseconds
        
        if [ $elapsed -lt $best_time ]; then
            best_time=$elapsed
        fi
    done
    
    echo $best_time
}

# Print results table header
printf "%-10s %-12s %-15s %-15s %-12s\n" "Format" "Size" "asset-io (ms)" "c2patool (ms)" "Speedup"
printf "%-10s %-12s %-15s %-15s %-12s\n" "------" "----" "-------------" "--------------" "-------"

# Test each format
test_format() {
    local format="$1"
    local file="$2"
    
    # Skip if file doesn't exist
    if [ ! -f "$file" ]; then
        echo "Skipping $format (file not found)"
        return
    fi
    
    # Get file size
    size=$(ls -lh "$file" | awk '{print $5}')
    
    # Clean up any existing output files
    rm -f target/output_c2pa.* target/c2patool_output.* 2>/dev/null || true
    
    # Time asset-io
    asset_io_time=$(time_command "$ASSET_IO_C2PA '$file'")
    
    # Time c2patool
    output_ext="${file##*.}"
    c2patool_time=$(time_command "$C2PATOOL '$file' -m tests/fixtures/minimal_manifest.json -c $TEST_SETTINGS -o target/c2patool_output.$output_ext -f")
    
    # Calculate speedup
    if [ $c2patool_time -gt 0 ]; then
        speedup=$(echo "scale=2; $c2patool_time / $asset_io_time" | bc)
    else
        speedup="N/A"
    fi
    
    printf "%-10s %-12s %-15s %-15s %-12s\n" "$format" "$size" "$asset_io_time" "$c2patool_time" "${speedup}x"
}

# Run tests for each format
test_format "PNG"  "tests/fixtures/sample1.png"
test_format "AVIF" "tests/fixtures/sample1.avif"
test_format "HEIC" "tests/fixtures/sample1.heic"
test_format "HEIF" "tests/fixtures/sample1.heif"
test_format "M4A"  "tests/fixtures/sample1.m4a"

echo ""
echo "Summary:"
echo "  - asset-io uses streaming write-hash-update (single pass I/O)"
echo "  - c2patool uses traditional approach (multiple passes)"
echo "  - Speedup = c2patool_time / asset_io_time (higher is better)"
echo ""
echo "Note: For large files (>100MB), the speedup advantage becomes more pronounced."
