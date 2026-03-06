# Benchmark Setup Summary

## 🎯 What's Available

You now have **two comprehensive benchmark tools** to compare c2patool vs c2pa_embeddable:

### 1. Shell Script: `benchmark_embeddable.sh`
- ✅ No dependencies needed
- ✅ Uses built-in `time` command
- ✅ Runs on any Unix system
- ✅ Automatic build and test

**Run it:**
```bash
cd /Users/gpeacock/dev/asset-io
./benchmark_embeddable.sh
```

### 2. Python Script: `benchmark_memory.py`
- 📊 Detailed memory profiling
- 📈 Real-time monitoring
- 🔥 CPU utilization tracking
- 💾 Peak and average memory usage

**Setup & Run:**
```bash
# Install psutil (one-time)
pip3 install psutil

# Run on your 6.7GB test file
python3 benchmark_memory.py tearsofsteel_4k.mov
```

## 📁 Files Created

- ✅ `benchmark_embeddable.sh` - Shell-based benchmark
- ✅ `benchmark_memory.py` - Python memory profiler
- ✅ `BENCHMARK_README.md` - Complete documentation

## 🚀 Quick Test

Want to do a quick test right now? Here's the fastest way:

```bash
cd /Users/gpeacock/dev/asset-io

# Option 1: Shell script (simpler, works immediately)
./benchmark_embeddable.sh

# Option 2: Python script (more detailed, requires psutil)
# First install psutil: pip3 install psutil
python3 benchmark_memory.py tearsofsteel_4k.mov
```

## 📊 Expected Results (6.7GB MOV)

Based on your previous benchmarks, you should see:

| Metric | c2patool | embeddable | Winner |
|--------|----------|------------|--------|
| **Time** | ~21.5s | ~9.1s | 🏆 **2.35x faster** |
| **System Time** | ~11.76s | ~4.00s | 🏆 **66% less I/O** |
| **Memory** | ~200MB | ~150MB | 🏆 **25% less** |
| **Throughput** | ~292 MB/s | ~687 MB/s | 🏆 **135% faster** |

## 🎬 What Happens When You Run

### Shell Script Flow:
1. Builds `c2pa_embeddable` in release mode
2. Runs c2patool on test file → measures time
3. Runs embeddable on test file → measures time
4. Compares results
5. Verifies both outputs with c2patool

### Python Script Flow:
1. Builds `c2pa_embeddable` in release mode (if needed)
2. Starts c2patool → monitors process every 100ms
3. Tracks: memory usage, CPU %, samples
4. Starts embeddable → monitors process every 100ms
5. Compares detailed metrics
6. Calculates efficiency scores

## 🔍 What You'll Learn

### Performance Insights:
- How much faster is the embeddable API?
- Where is the time spent? (user vs system time)
- Is I/O the bottleneck? (system time comparison)
- What's the throughput? (GB/s)

### Memory Insights:
- Peak memory usage
- Average memory over time
- Memory efficiency comparison
- Whether memory grows with file size

### Architecture Validation:
- Does streaming really avoid re-reading?
- Is in-place update actually faster?
- How much overhead is saved?

## 💡 Recommended Tests

### Test 1: Large File (Your 6.7GB MOV)
```bash
python3 benchmark_memory.py tearsofsteel_4k.mov
```
**Expected:** Embeddable 2-3x faster, much less system time

### Test 2: Medium File (If you have ~100MB file)
```bash
python3 benchmark_memory.py medium_video.mp4
```
**Expected:** Still faster, but smaller difference

### Test 3: Small File (< 10MB JPEG)
```bash
# Copy a test JPEG
cp /Users/gpeacock/dev/c2pa-rs/sdk/tests/fixtures/IMG_0003.jpg test.jpg
python3 benchmark_memory.py test.jpg
```
**Expected:** Similar performance (overhead dominates)

## 🐛 Troubleshooting

### "psutil not found"
```bash
pip3 install psutil
# or
python3 -m pip install psutil
```

### "Permission denied"
```bash
chmod +x benchmark_embeddable.sh
chmod +x benchmark_memory.py
```

### "c2patool not found"
```bash
cargo install c2patool
```

### Build fails
```bash
# Make sure you're in the asset-io directory
cd /Users/gpeacock/dev/asset-io

# Try building manually first
cargo build --release --example c2pa_embeddable --features all-formats,xmp
```

## 📝 Next Steps

1. **Install psutil** (for Python script):
   ```bash
   pip3 install psutil
   ```

2. **Run your first benchmark**:
   ```bash
   ./benchmark_embeddable.sh
   # or
   python3 benchmark_memory.py tearsofsteel_4k.mov
   ```

3. **Review results** and see the performance difference!

4. **Try different file sizes** to see how performance scales

## 📖 Full Documentation

See `BENCHMARK_README.md` for complete details on:
- What each metric means
- How to interpret results
- Advanced profiling techniques
- Contributing your results

---

**Ready to benchmark?** Just run:
```bash
./benchmark_embeddable.sh
```

Have fun seeing the 2-3x speedup! 🚀
