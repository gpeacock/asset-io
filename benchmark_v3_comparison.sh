#!/bin/bash

FILE="/Users/gpeacock/dev/asset-io/tearsofsteel_4k.mov"
MANIFEST="tests/fixtures/c2patool_manifest.json"

echo "=========================================="
echo "BMFF V3 Benchmark Comparison"
echo "File: $(basename $FILE) ($(ls -lh $FILE | awk '{print $5}'))"
echo "=========================================="
echo ""

# Clean up
rm -f target/output_c2pa.mov output_c2patool.mov 2>/dev/null

# Benchmark asset-io c2pa example (V3 sequential)
echo "🚀 Testing: asset-io + c2pa-rs (V3 Merkle, sequential)"
START=$(python3 -c 'import time; print(time.time())')
cargo run -q --release --example c2pa --features all-formats,xmp "$FILE" > /tmp/asset_io_output.txt 2>&1
END=$(python3 -c 'import time; print(time.time())')
ASSET_IO_TIME=$(python3 -c "print(f'{$END - $START:.2f}')")

# Show relevant output
tail -15 /tmp/asset_io_output.txt | grep -E "(Format|BmffHash|Hash|Success)" || tail -5 /tmp/asset_io_output.txt
echo "⏱️  Time: ${ASSET_IO_TIME}s"
echo ""

# Benchmark c2patool
echo "🔧 Testing: c2patool (standard)"
rm -f output_c2patool.mov 2>/dev/null
START=$(python3 -c 'import time; print(time.time())')
c2patool "$FILE" -m "$MANIFEST" -o output_c2patool.mov -f 2>&1 | tail -3 || true
END=$(python3 -c 'import time; print(time.time())')
C2PATOOL_TIME=$(python3 -c "print(f'{$END - $START:.2f}')")
echo "⏱️  Time: ${C2PATOOL_TIME}s"
echo ""

# Compare
echo "=========================================="
echo "📊 Results:"
echo "  asset-io (V3):  ${ASSET_IO_TIME}s"
echo "  c2patool:       ${C2PATOOL_TIME}s"
SPEEDUP=$(python3 -c "try:
    ratio = float($C2PATOOL_TIME) / float($ASSET_IO_TIME)
    if ratio > 1:
        print(f'{ratio:.2f}x faster')
    else:
        print(f'{1/ratio:.2f}x slower')
except:
    print('N/A')")
echo "  Difference:     ${SPEEDUP}"
echo "=========================================="
echo ""

# Verify both outputs
echo "🔍 Verification:"
if [ -f target/output_c2pa.mov ]; then
    echo "  asset-io output:"
    c2patool target/output_c2pa.mov 2>&1 | grep -E "(bmffHash|validation)" | grep -v "validationStatus" | head -3
fi
echo ""
if [ -f output_c2patool.mov ]; then
    echo "  c2patool output:"
    c2patool output_c2patool.mov 2>&1 | grep -E "(bmffHash|validation)" | grep -v "validationStatus" | head -3
fi
