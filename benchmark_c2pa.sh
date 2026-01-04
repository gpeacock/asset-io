#!/bin/bash
# C2PA Performance Benchmark: asset-io vs c2patool
# Compares signing performance across different file formats and sizes

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# High-resolution timer using Python
timer_start() {
    python3 -c "import time; print(time.time())"
}

timer_elapsed() {
    local start=$1
    local end=$(python3 -c "import time; print(time.time())")
    python3 -c "print($end - $start)"
}

echo -e "${BOLD}╔═══════════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║           C2PA Performance: asset-io vs c2patool                  ║${NC}"
echo -e "${BOLD}╚═══════════════════════════════════════════════════════════════════╝${NC}"
echo ""

# Build in release mode for fair comparison
echo -e "${YELLOW}📦 Building asset-io in release mode...${NC}"
cargo build --release --example c2pa --features all-formats,xmp --quiet
echo -e "${GREEN}✅ Build complete${NC}"
echo ""

# Test files array: name|path|mime_type
declare -a TEST_FILES=(
    "PNG (292K)|tests/fixtures/sample1.png|image/png"
    "JPEG (127K)|tests/fixtures/Designer.jpeg|image/jpeg"
    "AVIF (95K)|tests/fixtures/sample1.avif|image/avif"
    "HEIC (287K)|tests/fixtures/sample1.heic|image/heif"
    "HEIF (2.4M)|tests/fixtures/sample1.heif|image/heif"
    "M4A (3.8M)|tests/fixtures/sample1.m4a|audio/mp4"
    "MOV (164M)|$HOME/Downloads/Guest - Robert (06-24-25) (98th Birthday).mov|video/quicktime"
)

SETTINGS_FILE="tests/fixtures/test_settings.json"
C2PATOOL_MANIFEST="tests/fixtures/c2patool_manifest.json"

echo -e "${BOLD}Test Configuration:${NC}"
echo -e "  • Asset-io settings: $SETTINGS_FILE"
echo -e "  • c2patool manifest: $C2PATOOL_MANIFEST"
echo -e "  • Iterations: 3 per tool"
echo -e "  • Mode: Release (optimized)"
echo ""

# Results file
RESULTS_FILE=$(mktemp)

for test_file in "${TEST_FILES[@]}"; do
    IFS='|' read -r name path mime_type <<< "$test_file"
    
    # Skip if file doesn't exist
    if [ ! -f "$path" ]; then
        echo -e "${YELLOW}⚠️  Skipping $name (file not found)${NC}"
        echo ""
        continue
    fi
    
    echo -e "${BOLD}${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BOLD}Testing: $name${NC}"
    echo -e "${BOLD}${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    
    # Get file size
    file_size=$(ls -lh "$path" | awk '{print $5}')
    echo -e "  File: $path"
    echo -e "  Size: $file_size"
    echo -e "  MIME: $mime_type"
    echo ""
    
    # Clean up old output files
    rm -f target/output_c2pa.* target/benchmark_c2patool_output.* 2>/dev/null || true
    
    # Test asset-io (3 runs)
    echo -e "${GREEN}🚀 asset-io (3 runs):${NC}"
    asset_io_total=0
    for i in {1..3}; do
        start=$(timer_start)
        ./target/release/examples/c2pa "$path" >/dev/null 2>&1
        elapsed=$(timer_elapsed $start)
        asset_io_total=$(python3 -c "print($asset_io_total + $elapsed)")
        printf "  Run %d: %.3f seconds\n" "$i" "$elapsed"
    done
    asset_io_avg=$(python3 -c "print($asset_io_total / 3)")
    echo -e "  ${BOLD}Average: ${asset_io_avg} seconds${NC}"
    echo ""
    
    # Clean up
    rm -f target/output_c2pa.* 2>/dev/null || true
    
    # Test c2patool (3 runs)
    echo -e "${BLUE}🔧 c2patool (3 runs):${NC}"
    c2patool_total=0
    
    # Determine output extension
    ext="${path##*.}"
    output_path="target/benchmark_c2patool_output.$ext"
    
    c2patool_failed=0
    for i in {1..3}; do
        start=$(timer_start)
        if ! c2patool "$path" \
            --manifest "$C2PATOOL_MANIFEST" \
            --output "$output_path" \
            --force >/dev/null 2>&1; then
            echo -e "${RED}  ❌ c2patool failed${NC}"
            c2patool_failed=1
            break
        fi
        elapsed=$(timer_elapsed $start)
        c2patool_total=$(python3 -c "print($c2patool_total + $elapsed)")
        printf "  Run %d: %.3f seconds\n" "$i" "$elapsed"
        rm -f "$output_path" 2>/dev/null || true
    done
    
    if [ $c2patool_failed -eq 0 ]; then
        c2patool_avg=$(python3 -c "print($c2patool_total / 3)")
        echo -e "  ${BOLD}Average: ${c2patool_avg} seconds${NC}"
        
        # Calculate speedup
        speedup=$(python3 -c "print(round($c2patool_avg / $asset_io_avg, 2))")
        is_faster=$(python3 -c "print(1 if $speedup > 1 else 0)")
        
        if [ "$is_faster" -eq "1" ]; then
            echo -e "  ${GREEN}${BOLD}⚡ asset-io is ${speedup}x faster${NC}"
        else
            inverse=$(python3 -c "print(round(1 / $speedup, 2))")
            echo -e "  ${RED}c2patool is ${inverse}x faster${NC}"
        fi
        
        # Save results
        echo "$name|$asset_io_avg|$c2patool_avg|$speedup" >> "$RESULTS_FILE"
    else
        # Save error result
        echo "$name|$asset_io_avg|ERROR|0" >> "$RESULTS_FILE"
    fi
    
    echo ""
done

# Summary table
echo -e "${BOLD}╔═══════════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║                        PERFORMANCE SUMMARY                        ║${NC}"
echo -e "${BOLD}╚═══════════════════════════════════════════════════════════════════╝${NC}"
echo ""
printf "${BOLD}%-20s  %12s  %12s  %10s${NC}\n" "Format" "asset-io" "c2patool" "Speedup"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

while IFS='|' read -r name asset_io_time c2patool_time speedup; do
    if [ "$c2patool_time" == "ERROR" ]; then
        printf "%-20s  ${GREEN}%12ss${NC}  ${RED}%12s${NC}  %10s\n" \
            "$name" "$asset_io_time" "FAILED" "-"
    else
        is_faster=$(python3 -c "print(1 if $speedup > 1 else 0)")
        color="${GREEN}"
        if [ "$is_faster" -eq "0" ]; then
            color="${RED}"
        fi
        printf "%-20s  ${GREEN}%12ss${NC}  ${BLUE}%12ss${NC}  ${color}%9sx${NC}\n" \
            "$name" "$asset_io_time" "$c2patool_time" "$speedup"
    fi
done < "$RESULTS_FILE"

echo ""
echo -e "${BOLD}${GREEN}✅ Benchmark complete!${NC}"

# Clean up
rm -f "$RESULTS_FILE"
