#!/bin/bash
# Benchmark comparing c2patool vs c2pa_embeddable example
# Tests performance and memory usage on various file sizes

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "🚀 C2PA Embeddable API Benchmark"
echo "=================================="
echo ""

# Check dependencies
if ! command -v c2patool &> /dev/null; then
    echo -e "${RED}Error: c2patool not found${NC}"
    exit 1
fi

if ! command -v /usr/bin/time &> /dev/null; then
    echo -e "${YELLOW}Warning: GNU time not found, using basic timing${NC}"
    USE_GNU_TIME=false
else
    USE_GNU_TIME=true
fi

# Configuration
MANIFEST_JSON="tests/fixtures/minimal_manifest.json"
SETTINGS_JSON="tests/fixtures/test_settings.json"

# Test files (add your own)
TEST_FILES=(
    "tearsofsteel_4k.mov:video/quicktime:6.7GB"
)

# Build the example in release mode
echo -e "${BLUE}Building c2pa_embeddable in release mode...${NC}"
cargo build --release --example c2pa_embeddable --features all-formats,xmp
echo ""

# Function to get file size in bytes
get_file_size() {
    stat -f%z "$1" 2>/dev/null || stat -c%s "$1" 2>/dev/null
}

# Function to format bytes
format_bytes() {
    local bytes=$1
    if [ $bytes -gt 1073741824 ]; then
        echo "$(echo "scale=2; $bytes/1073741824" | bc)GB"
    elif [ $bytes -gt 1048576 ]; then
        echo "$(echo "scale=2; $bytes/1048576" | bc)MB"
    elif [ $bytes -gt 1024 ]; then
        echo "$(echo "scale=2; $bytes/1024" | bc)KB"
    else
        echo "${bytes}B"
    fi
}

# Function to benchmark with detailed memory tracking
benchmark_tool() {
    local tool_name=$1
    local input=$2
    local output=$3
    local format=$4
    
    echo -e "${GREEN}Testing: $tool_name${NC}"
    
    # Clean output file
    rm -f "$output"
    
    # Run with time tracking
    local start=$(date +%s)
    
    if [ "$tool_name" = "c2patool" ]; then
        /usr/bin/time -l c2patool "$input" --output "$output" --manifest "$MANIFEST_JSON" --force 2>&1 | tee /tmp/benchmark_output.txt
    else
        /usr/bin/time -l ./target/release/examples/c2pa_embeddable "$input" "$output" 2>&1 | tee /tmp/benchmark_output.txt
    fi
    
    local end=$(date +%s)
    local duration=$((end - start))
    
    # Parse macOS time output
    local user_time=$(grep "user" /tmp/benchmark_output.txt | awk '{print $1}')
    local sys_time=$(grep "sys" /tmp/benchmark_output.txt | awk '{print $1}')
    local max_mem=$(grep "maximum resident set size" /tmp/benchmark_output.txt | awk '{print $1}')
    
    echo "  Time: ${duration}s"
    echo "  User: ${user_time}s"
    echo "  Sys:  ${sys_time}s"
    echo "  Peak Memory: $(format_bytes $max_mem)"
    
    # Verify output exists and get size
    if [ -f "$output" ]; then
        local out_size=$(get_file_size "$output")
        echo "  Output: $(format_bytes $out_size)"
        echo -e "  ${GREEN}✓ Success${NC}"
    else
        echo -e "  ${RED}✗ Failed - output not created${NC}"
        return 1
    fi
    
    echo ""
}

# Create manifest if needed
if [ ! -f "$MANIFEST_JSON" ]; then
    echo -e "${YELLOW}Creating minimal manifest...${NC}"
    cat > /tmp/minimal_manifest.json << 'EOF'
{
  "claim_generator": "benchmark-test/1.0",
  "assertions": [
    {
      "label": "c2pa.actions",
      "data": {
        "actions": [
          {
            "action": "c2pa.created"
          }
        ]
      }
    }
  ]
}
EOF
    MANIFEST_JSON="/tmp/minimal_manifest.json"
fi

# Main benchmark loop
echo "🎯 Starting Benchmarks"
echo "====================="
echo ""

for test_file_spec in "${TEST_FILES[@]}"; do
    IFS=':' read -r file format size <<< "$test_file_spec"
    
    if [ ! -f "$file" ]; then
        echo -e "${YELLOW}Skipping $file (not found)${NC}"
        echo ""
        continue
    fi
    
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}File: $file${NC}"
    echo -e "${BLUE}Size: $size${NC}"
    echo -e "${BLUE}Format: $format${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    
    # Benchmark c2patool
    echo "1️⃣  c2patool (traditional workflow)"
    echo "   - Full file write"
    echo "   - Re-read for hashing"
    echo "   - Full file rewrite with manifest"
    echo ""
    benchmark_tool "c2patool" "$file" "output_c2patool.${file##*.}" "$format"
    
    # Benchmark embeddable
    echo "2️⃣  c2pa_embeddable (streaming workflow)"
    echo "   - Single-pass write with placeholder"
    echo "   - Hash during write"
    echo "   - In-place JUMBF update"
    echo ""
    benchmark_tool "embeddable" "$file" "output_embeddable.${file##*.}" "$format"
    
    # Compare file sizes
    echo "📊 Comparison"
    echo "   c2patool output:     $(format_bytes $(get_file_size "output_c2patool.${file##*.}"))"
    echo "   embeddable output:   $(format_bytes $(get_file_size "output_embeddable.${file##*.}"))"
    
    # Verify both files
    echo ""
    echo "🔍 Verification"
    echo "   Checking c2patool output..."
    if c2patool "output_c2patool.${file##*.}" --info > /dev/null 2>&1; then
        echo -e "   ${GREEN}✓ c2patool output verified${NC}"
    else
        echo -e "   ${RED}✗ c2patool output invalid${NC}"
    fi
    
    echo "   Checking embeddable output..."
    if c2patool "output_embeddable.${file##*.}" --info > /dev/null 2>&1; then
        echo -e "   ${GREEN}✓ embeddable output verified${NC}"
    else
        echo -e "   ${RED}✗ embeddable output invalid${NC}"
    fi
    
    echo ""
    echo ""
done

echo "✨ Benchmark Complete!"
echo ""
echo "💡 Key Metrics:"
echo "   - Time: Total execution time"
echo "   - User: CPU time in user mode"
echo "   - Sys:  CPU time in kernel mode (I/O operations)"
echo "   - Peak Memory: Maximum memory used"
echo ""
echo "📝 Note: Lower 'Sys' time indicates better I/O efficiency"
echo "         (embeddable should have ~3x less system time)"
