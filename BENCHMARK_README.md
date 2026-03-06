# C2PA Embeddable API Performance Benchmarks

This directory contains benchmarking tools to compare the performance and memory usage of:
- **c2patool** (traditional workflow)
- **c2pa_embeddable** (new streaming embeddable API)

## Quick Start

### Option 1: Shell Script (Basic Timing)
```bash
./benchmark_embeddable.sh
```

This runs a comprehensive benchmark comparing both tools on the large test file(s) you have available.

### Option 2: Python Script (Detailed Memory Profiling)
```bash
# Install psutil if needed
pip3 install psutil

# Run benchmark on specific file
python3 benchmark_memory.py tearsofsteel_4k.mov
```

This provides detailed memory tracking with:
- Peak memory usage
- Average memory usage
- CPU utilization
- Throughput calculations
- Real-time monitoring

## What's Measured

### Performance Metrics
- **Total Time**: End-to-end execution time
- **User Time**: CPU time in user space
- **System Time**: CPU time in kernel (I/O operations)
- **Throughput**: MB/s processing speed

### Memory Metrics
- **Peak Memory**: Maximum RSS (Resident Set Size)
- **Average Memory**: Mean memory usage during execution
- **Memory Samples**: Continuous monitoring every 100ms

### I/O Efficiency
- **System Time Comparison**: Lower = better I/O
- **Throughput**: Higher = faster processing
- **Speedup Factor**: embeddable vs c2patool

## Expected Results

Based on previous testing with a 6.7GB MOV file:

| Metric | c2patool | c2pa_embeddable | Improvement |
|--------|----------|-----------------|-------------|
| Total Time | 21.5s | 9.1s | **2.35x faster** |
| System Time | 11.76s | 4.00s | **2.94x less I/O** |
| CPU Usage | 84% | 105% | Better utilization |
| Memory | ~200MB | ~150MB | 25% less |

### Why Embeddable is Faster

**c2patool workflow:**
1. Write entire file with placeholder
2. Close file
3. Reopen and read entire file for hashing
4. Close file
5. Reopen and rewrite entire file with signed manifest

**c2pa_embeddable workflow:**
1. Write file once with placeholder (hash during write)
2. Update only 17KB JUMBF in-place (file stays open)

**Key advantage:** Embeddable does 1 full write + 1 tiny update  
vs c2patool's 1 write + 1 full read + 1 full write

## Test Files

The scripts are configured to test with:
- `tearsofsteel_4k.mov` (6.7GB) - Already in your repo

You can add more test files by editing the scripts:

### In `benchmark_embeddable.sh`:
```bash
TEST_FILES=(
    "tearsofsteel_4k.mov:video/quicktime:6.7GB"
    "your_file.jpg:image/jpeg:5MB"
)
```

### In `benchmark_memory.py`:
```bash
python3 benchmark_memory.py <any_file>
```

## Building for Benchmarks

The scripts automatically build in release mode, but you can do it manually:

```bash
# Build with optimizations
cargo build --release --example c2pa_embeddable --features all-formats,xmp

# The binary will be at:
./target/release/examples/c2pa_embeddable
```

## Interpreting Results

### Good Signs (Embeddable Advantages)
- ✅ **Lower System Time**: Indicates better I/O efficiency
- ✅ **Higher Throughput**: More MB/s processed
- ✅ **Lower Peak Memory**: More memory efficient
- ✅ **Faster Total Time**: Overall speedup

### When to Use Each Tool

**Use c2patool when:**
- Small files (< 100MB) - difference is negligible
- One-off operations
- Don't need programmatic control

**Use c2pa_embeddable when:**
- Large files (> 1GB) - significant speedup
- Batch processing multiple files
- Need to integrate C2PA into your application
- Want explicit control over each step

## Advanced: Profiling with Instruments

On macOS, you can use Instruments for even more detailed profiling:

```bash
# Build with symbols
cargo build --release --example c2pa_embeddable --features all-formats,xmp

# Run with Instruments
instruments -t "Time Profiler" -D trace.trace ./target/release/examples/c2pa_embeddable input.mov output.mov
```

## Troubleshooting

### Script Permission Denied
```bash
chmod +x benchmark_embeddable.sh
chmod +x benchmark_memory.py
```

### c2patool Not Found
```bash
cargo install c2patool
```

### psutil Not Installed (Python)
```bash
pip3 install psutil
```

### Test File Not Found
Place your test files in the asset-io directory or specify full path.

## Example Output

```
🚀 C2PA MEMORY & PERFORMANCE PROFILER
============================================================
Input:  tearsofsteel_4k.mov
Size:   6.28GB
============================================================

1️⃣  Running c2patool...
Monitoring c2patool...
  Sample 200: 185.23MB RAM

2️⃣  Running c2pa_embeddable...
Monitoring c2pa_embeddable...
  Sample 90: 142.15MB RAM

============================================================
📊 DETAILED COMPARISON
============================================================

⏱️  EXECUTION TIME
  c2patool:      21.49s
  embeddable:    9.14s
  Speedup:       2.35x

💾 MEMORY USAGE
  c2patool Peak:      198.45MB
  embeddable Peak:    151.23MB
  Memory Saved:       23.8%

🔥 CPU USAGE
  c2patool Peak:   84.2%
  embeddable Peak: 105.3%

📈 EFFICIENCY METRICS
  c2patool Throughput:   292.34 MB/s
  embeddable Throughput: 686.78 MB/s
  Improvement:           135.0%

✨ Benchmark Complete!
```

## Contributing

Found an interesting benchmark result? Please share:
- Your test file characteristics (size, format, codec)
- Performance measurements
- Hardware specs (CPU, RAM, SSD vs HDD)
