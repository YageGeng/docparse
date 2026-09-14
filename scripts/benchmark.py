"""Run the native corpus benchmark with process and GPU telemetry through uv's dev group."""

import argparse
import csv
import json
import os
from pathlib import Path
import subprocess
import time

import psutil


def run_case(binary: Path, config: Path, pdfs: Path, output: Path, concurrency: int, repeats: int) -> int:
    """Run one isolated model instance and retain telemetry even if inference fails."""
    prefix = output / f"c{concurrency}"
    fields = "timestamp,memory.used,utilization.gpu,utilization.memory,temperature.gpu,power.draw,clocks.sm,clocks.mem"
    with prefix.with_suffix(".gpu.csv").open("w") as gpu_log, prefix.with_suffix(".stderr.log").open("w") as log, prefix.with_suffix(".process.csv").open("w") as process_log:
        gpu_log.write(fields + "\n")
        gpu_log.flush()
        gpu = subprocess.Popen(["nvidia-smi", f"--query-gpu={fields}", "--format=csv,noheader,nounits", "--loop-ms=1000"], stdout=gpu_log, stderr=log)
        process = None
        try:
            process = subprocess.Popen([str(binary), str(config), str(pdfs), str(prefix.with_suffix(".metrics.jsonl")), str(concurrency), str(repeats)], stdout=log, stderr=log, env={**os.environ, "RUST_LOG": "warn,ort=error"})
            probe = psutil.Process(process.pid)
            probe.cpu_percent()
            # Keep generated telemetry consistent with the repository's LF line endings.
            writer = csv.writer(process_log, lineterminator="\n")
            writer.writerow(["unix_seconds", "pid", "cpu_percent", "rss_mb", "threads", "read_bytes", "write_bytes", "voluntary_switches", "involuntary_switches"])
            print(f"started concurrency={concurrency}, repeats={repeats}, pid={process.pid}", flush=True)
            while process.poll() is None:
                try:
                    with probe.oneshot():
                        memory = probe.memory_info()
                        io = probe.io_counters()
                        switches = probe.num_ctx_switches()
                        writer.writerow([time.time(), process.pid, probe.cpu_percent(), memory.rss / 2**20, probe.num_threads(), io.read_bytes, io.write_bytes, switches.voluntary, switches.involuntary])
                    process_log.flush()
                except (psutil.NoSuchProcess, psutil.ZombieProcess):
                    break
                time.sleep(1)
            status = process.wait()
            print(f"finished concurrency={concurrency}, exit={status}", flush=True)
            return status
        finally:
            # Interrupting the monitor must not leave a hidden benchmark or GPU sampler running.
            for child in [process, gpu]:
                if child is not None and child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()


def main() -> None:
    """Run complete-corpus concurrency cases and record the reproducible invocation without database credentials."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/examples/benchmark"))
    parser.add_argument("--config", type=Path, default=Path("docparse.toml"))
    parser.add_argument("--pdf-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--concurrency", type=int, nargs="+", default=[1, 2, 4])
    parser.add_argument("--repeats", type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10 or any(not 1 <= count <= 32 for count in args.concurrency):
        parser.error("repeats must be 1..10 and concurrency 1..32")
    if len(set(args.concurrency)) != len(args.concurrency):
        parser.error("concurrency values must be unique to avoid overwriting trial artifacts")
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    binary, config, pdfs = args.binary.resolve(), args.config.resolve(), args.pdf_dir.resolve()
    cases = []
    for concurrency in args.concurrency:
        status = run_case(binary, config, pdfs, output, concurrency, args.repeats)
        cases.append({"concurrency": concurrency, "exit_code": status})
        (output / "cases.json").write_text(json.dumps(cases, indent=2) + "\n")
    if any(case["exit_code"] != 0 for case in cases):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
