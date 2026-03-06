#!/usr/bin/env python3
"""
Memory and performance profiler for C2PA signing tools
Uses psutil for detailed memory tracking
"""

import subprocess
import time
import sys
import os
from pathlib import Path

try:
    import psutil
except ImportError:
    print("❌ Error: psutil not installed")
    print("Install with: pip3 install psutil")
    sys.exit(1)

class Colors:
    BLUE = '\033[0;34m'
    GREEN = '\033[0;32m'
    YELLOW = '\033[1;33m'
    RED = '\033[0;31m'
    NC = '\033[0m'

def format_bytes(bytes_val):
    """Format bytes to human readable"""
    for unit in ['B', 'KB', 'MB', 'GB']:
        if bytes_val < 1024.0:
            return f"{bytes_val:.2f}{unit}"
        bytes_val /= 1024.0
    return f"{bytes_val:.2f}TB"

def format_time(seconds):
    """Format seconds to human readable"""
    if seconds < 60:
        return f"{seconds:.2f}s"
    minutes = int(seconds // 60)
    secs = seconds % 60
    return f"{minutes}m {secs:.1f}s"

def monitor_process(process, tool_name):
    """Monitor a process and collect memory/CPU stats"""
    print(f"{Colors.GREEN}Monitoring {tool_name}...{Colors.NC}")
    
    stats = {
        'peak_memory': 0,
        'peak_memory_mb': 0,
        'avg_memory': 0,
        'samples': 0,
        'cpu_percent': 0,
        'duration': 0,
    }
    
    start_time = time.time()
    memory_samples = []
    
    try:
        proc = psutil.Process(process.pid)
        
        while process.poll() is None:
            try:
                mem_info = proc.memory_info()
                memory_samples.append(mem_info.rss)
                stats['peak_memory'] = max(stats['peak_memory'], mem_info.rss)
                stats['samples'] += 1
                
                # Get CPU percent
                cpu = proc.cpu_percent(interval=0.1)
                stats['cpu_percent'] = max(stats['cpu_percent'], cpu)
                
                # Print progress
                if stats['samples'] % 10 == 0:
                    print(f"  Sample {stats['samples']}: {format_bytes(mem_info.rss)} RAM", end='\r')
                
                time.sleep(0.1)
                
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                break
        
        stats['duration'] = time.time() - start_time
        
        if memory_samples:
            stats['avg_memory'] = sum(memory_samples) / len(memory_samples)
            stats['peak_memory_mb'] = stats['peak_memory'] / (1024 * 1024)
        
        print()  # Clear the progress line
        
    except Exception as e:
        print(f"{Colors.RED}Error monitoring: {e}{Colors.NC}")
    
    return stats

def run_c2patool(input_file, output_file, manifest_file):
    """Run c2patool and monitor it"""
    print(f"\n{Colors.BLUE}1️⃣  Running c2patool...{Colors.NC}")
    
    cmd = [
        'c2patool',
        input_file,
        '--output', output_file,
        '--manifest', manifest_file,
        '--force'
    ]
    
    start = time.time()
    process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stats = monitor_process(process, 'c2patool')
    stdout, stderr = process.communicate()
    
    if process.returncode != 0:
        print(f"{Colors.RED}Error: {stderr.decode()}{Colors.NC}")
        return None
    
    stats['duration'] = time.time() - start
    return stats

def run_embeddable(input_file, output_file, example_path):
    """Run c2pa_embeddable example and monitor it"""
    print(f"\n{Colors.BLUE}2️⃣  Running c2pa_embeddable...{Colors.NC}")
    
    cmd = [example_path, input_file, output_file]
    
    start = time.time()
    process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stats = monitor_process(process, 'c2pa_embeddable')
    stdout, stderr = process.communicate()
    
    if process.returncode != 0:
        print(f"{Colors.RED}Error: {stderr.decode()}{Colors.NC}")
        return None
    
    stats['duration'] = time.time() - start
    return stats

def print_comparison(c2patool_stats, embeddable_stats):
    """Print detailed comparison"""
    print(f"\n{Colors.BLUE}{'='*60}{Colors.NC}")
    print(f"{Colors.BLUE}📊 DETAILED COMPARISON{Colors.NC}")
    print(f"{Colors.BLUE}{'='*60}{Colors.NC}\n")
    
    # Time comparison
    print("⏱️  EXECUTION TIME")
    print(f"  c2patool:      {format_time(c2patool_stats['duration'])}")
    print(f"  embeddable:    {format_time(embeddable_stats['duration'])}")
    speedup = c2patool_stats['duration'] / embeddable_stats['duration']
    color = Colors.GREEN if speedup > 1 else Colors.YELLOW
    print(f"  {color}Speedup:       {speedup:.2f}x{Colors.NC}\n")
    
    # Memory comparison
    print("💾 MEMORY USAGE")
    print(f"  c2patool Peak:      {format_bytes(c2patool_stats['peak_memory'])}")
    print(f"  embeddable Peak:    {format_bytes(embeddable_stats['peak_memory'])}")
    print(f"  c2patool Average:   {format_bytes(c2patool_stats['avg_memory'])}")
    print(f"  embeddable Average: {format_bytes(embeddable_stats['avg_memory'])}")
    
    mem_savings = (c2patool_stats['peak_memory'] - embeddable_stats['peak_memory']) / c2patool_stats['peak_memory'] * 100
    if mem_savings > 0:
        print(f"  {Colors.GREEN}Memory Saved:       {mem_savings:.1f}%{Colors.NC}\n")
    else:
        print(f"  {Colors.YELLOW}Memory Difference:  {abs(mem_savings):.1f}% more{Colors.NC}\n")
    
    # CPU comparison
    print("🔥 CPU USAGE")
    print(f"  c2patool Peak:   {c2patool_stats['cpu_percent']:.1f}%")
    print(f"  embeddable Peak: {embeddable_stats['cpu_percent']:.1f}%\n")
    
    # Efficiency metrics
    print("📈 EFFICIENCY METRICS")
    c2pa_throughput = os.path.getsize(sys.argv[1]) / c2patool_stats['duration'] / (1024*1024)
    emb_throughput = os.path.getsize(sys.argv[1]) / embeddable_stats['duration'] / (1024*1024)
    print(f"  c2patool Throughput:   {c2pa_throughput:.2f} MB/s")
    print(f"  embeddable Throughput: {emb_throughput:.2f} MB/s")
    print(f"  {Colors.GREEN}Improvement:           {(emb_throughput/c2pa_throughput - 1)*100:.1f}%{Colors.NC}\n")

def main():
    if len(sys.argv) < 2:
        print("Usage: python3 benchmark_memory.py <input_file>")
        print("\nExample:")
        print("  python3 benchmark_memory.py tearsofsteel_4k.mov")
        sys.exit(1)
    
    input_file = sys.argv[1]
    
    if not os.path.exists(input_file):
        print(f"{Colors.RED}Error: File not found: {input_file}{Colors.NC}")
        sys.exit(1)
    
    file_size = os.path.getsize(input_file)
    
    print(f"\n{Colors.BLUE}🚀 C2PA MEMORY & PERFORMANCE PROFILER{Colors.NC}")
    print(f"{Colors.BLUE}{'='*60}{Colors.NC}")
    print(f"Input:  {input_file}")
    print(f"Size:   {format_bytes(file_size)}")
    print(f"{Colors.BLUE}{'='*60}{Colors.NC}\n")
    
    # Check for required files
    manifest_file = 'tests/fixtures/minimal_manifest.json'
    if not os.path.exists(manifest_file):
        # Create a minimal manifest
        manifest_file = '/tmp/minimal_manifest.json'
        with open(manifest_file, 'w') as f:
            f.write('''
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
''')
    
    example_path = './target/release/examples/c2pa_embeddable'
    if not os.path.exists(example_path):
        print(f"{Colors.YELLOW}Building c2pa_embeddable in release mode...{Colors.NC}")
        subprocess.run(['cargo', 'build', '--release', '--example', 'c2pa_embeddable', '--features', 'all-formats,xmp'])
    
    # Run benchmarks
    c2patool_output = 'output_c2patool_mem.' + input_file.split('.')[-1]
    embeddable_output = 'output_embeddable_mem.' + input_file.split('.')[-1]
    
    # Clean up old outputs
    for f in [c2patool_output, embeddable_output]:
        if os.path.exists(f):
            os.remove(f)
    
    c2patool_stats = run_c2patool(input_file, c2patool_output, manifest_file)
    embeddable_stats = run_embeddable(input_file, embeddable_output, example_path)
    
    if c2patool_stats and embeddable_stats:
        print_comparison(c2patool_stats, embeddable_stats)
        
        # Verify outputs
        print(f"{Colors.BLUE}🔍 VERIFICATION{Colors.NC}")
        for tool, output in [('c2patool', c2patool_output), ('embeddable', embeddable_output)]:
            if os.path.exists(output):
                size = os.path.getsize(output)
                print(f"  {tool}: {format_bytes(size)} {Colors.GREEN}✓{Colors.NC}")
            else:
                print(f"  {tool}: {Colors.RED}Output not found ✗{Colors.NC}")
    
    print(f"\n{Colors.GREEN}✨ Benchmark Complete!{Colors.NC}\n")

if __name__ == '__main__':
    main()
