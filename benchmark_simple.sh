#!/bin/bash
# Simple, reliable benchmark for macOS
# Just compares execution time and basic stats

set -e

echo "🚀 C2PA Quick Benchmark"
echo "======================="
echo ""

INPUT_FILE="${1:-tearsofsteel_4k.mov}"

if [ ! -f "$INPUT_FILE" ]; then
    echo "❌ File not found: $INPUT_FILE"
    echo "Usage: $0 <input_file>"
    exit 1
fi

FILE_SIZE=$(stat -f%z "$INPUT_FILE")
echo "Input: $INPUT_FILE"
echo "Size:  $(echo "scale=2; $FILE_SIZE/1073741824" | bc)GB"
echo ""

# Build if needed
if [ ! -f ./target/release/examples/c2pa_embeddable ]; then
    echo "📦 Building c2pa_embeddable..."
    cargo build --release --example c2pa_embeddable --features all-formats,xmp --quiet
    echo ""
fi

# Create manifest
cat > /tmp/minimal_manifest.json << 'EOF'
{
  "claim_generator": "benchmark-test/1.0",
  "assertions": [
    {
      "label": "c2pa.actions",
      "data": {
        "actions": [{"action": "c2pa.created"}]
      }
    }
  ]
}
EOF

echo "1️⃣  Testing c2patool..."
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
rm -f output_c2patool.mov
/usr/bin/time -l c2patool "$INPUT_FILE" \
    --output output_c2patool.mov \
    --manifest /tmp/minimal_manifest.json \
    --force \
    2>&1 | grep -E "real|user|sys|maximum resident"
echo ""

echo "2️⃣  Testing c2pa_embeddable..."
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
rm -f output_embeddable.mov
/usr/bin/time -l ./target/release/examples/c2pa_embeddable \
    "$INPUT_FILE" output_embeddable.mov \
    2>&1 | grep -E "real|user|sys|maximum resident"
echo ""

echo "📊 Results:"
if [ -f output_c2patool.mov ] && [ -f output_embeddable.mov ]; then
    C2PA_SIZE=$(stat -f%z output_c2patool.mov)
    EMB_SIZE=$(stat -f%z output_embeddable.mov)
    echo "  c2patool output:   $(echo "scale=2; $C2PA_SIZE/1048576" | bc)MB"
    echo "  embeddable output: $(echo "scale=2; $EMB_SIZE/1048576" | bc)MB"
    echo ""
    echo "✅ Both tools succeeded!"
else
    echo "❌ One or both tools failed"
fi

echo ""
echo "💡 Look for:"
echo "   - Lower 'real' time = faster overall"
echo "   - Lower 'sys' time = better I/O efficiency"
echo "   - Lower 'maximum resident' = less memory"
