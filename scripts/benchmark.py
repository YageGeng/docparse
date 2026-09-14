"""Run the native corpus benchmark with per-process and optional GPU telemetry."""

import argparse
import csv
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

import psutil


def run_case(binary: Path, config: Path, pdfs: Path, output: Path, concurrency: int, repeats: int, processes: int | None) -> int:
    """Measure the complete process family and reap owned descendants on interruption."""
    prefix = output / (f"p{processes}-c{concurrency}" if processes is not None else f"c{concurrency}")
    fields = "timestamp,memory.used,utilization.gpu,utilization.memory,temperature.gpu,power.draw,clocks.sm,clocks.mem"
    probes: dict[int, psutil.Process] = {}
    process = gpu = None
    with prefix.with_suffix(".gpu.csv").open("w") as gpu_log, prefix.with_suffix(".stderr.log").open("w") as log, prefix.with_suffix(".process.csv").open("w") as process_log:
        gpu_log.write(fields + "\n")
        gpu_log.flush()
        try:
            if shutil.which("nvidia-smi"):
                gpu = subprocess.Popen(["nvidia-smi", f"--query-gpu={fields}", "--format=csv,noheader,nounits", "--loop-ms=1000"], stdout=gpu_log, stderr=log)
            else:
                print("GPU telemetry unavailable; the benchmark records its actual inference backend", flush=True)
            command = [str(binary), str(config), str(pdfs), str(prefix.with_suffix(".metrics.jsonl")), str(concurrency), str(repeats)]
            if processes is not None:
                command.append(str(processes))
            process = subprocess.Popen(command, stdout=log, stderr=log, env={**os.environ, "RUST_LOG": "warn,ort=error"})
            root = psutil.Process(process.pid)
            writer = csv.writer(process_log, lineterminator="\n")
            writer.writerow(["unix_seconds", "pid", "parent_pid", "cpu_percent", "rss_mb", "threads", "read_bytes", "write_bytes", "voluntary_switches", "involuntary_switches"])
            print(f"started processes={processes}, concurrency={concurrency}, repeats={repeats}, pid={process.pid}", flush=True)
            while process.poll() is None:
                try:
                    family = [root, *root.children(recursive=True)]
                except (psutil.NoSuchProcess, psutil.ZombieProcess):
                    break
                for observed in family:
                    try:
                        fresh = observed.pid not in probes
                        if fresh:
                            probes[observed.pid] = observed
                            observed.cpu_percent()
                        probe = probes[observed.pid]
                        with probe.oneshot():
                            memory = probe.memory_info()
                            io = probe.io_counters() if hasattr(probe, "io_counters") else None
                            switches = probe.num_ctx_switches()
                            writer.writerow([time.time(), probe.pid, probe.ppid(), None if fresh else probe.cpu_percent(), memory.rss / 2**20, probe.num_threads(), None if io is None else io.read_bytes, None if io is None else io.write_bytes, switches.voluntary, switches.involuntary])
                    except (psutil.NoSuchProcess, psutil.ZombieProcess):
                        continue
                process_log.flush()
                time.sleep(1)
            status = process.wait()
            print(f"finished processes={processes}, concurrency={concurrency}, exit={status}", flush=True)
            return status
        finally:
            # Capture descendants before stopping the parent, then keep PID identity via psutil handles.
            if process is not None and process.poll() is None:
                try:
                    for child in psutil.Process(process.pid).children(recursive=True):
                        probes.setdefault(child.pid, child)
                except psutil.NoSuchProcess:
                    pass
            # Stop the supervisor before its workers: killing workers first lets the live pool
            # replace them after the descendant snapshot, leaking untracked orphan processes.
            for child in [process, gpu]:
                if child is not None and child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()
            descendants = [probe for pid, probe in probes.items() if process is not None and pid != process.pid]
            for child in descendants:
                try:
                    child.terminate()
                except psutil.NoSuchProcess:
                    pass
            _, alive = psutil.wait_procs(descendants, timeout=5)
            for child in alive:
                try:
                    child.kill()
                    child.wait(timeout=5)
                except psutil.NoSuchProcess:
                    pass


def main() -> None:
    """Compare independent process-count and document-concurrency controls without changing model coverage."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/examples/pdfium_benchmark"))
    parser.add_argument("--config", type=Path, default=Path("docparse.toml"))
    parser.add_argument("--pdf-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--concurrency", type=int, nargs="+", default=[1, 2, 4])
    parser.add_argument("--pdfium-processes", type=int, nargs="+", help="override the configured PDFium process count for each case")
    parser.add_argument("--repeats", type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10 or any(not 1 <= count <= 32 for count in args.concurrency):
        parser.error("repeats must be 1..10 and concurrency 1..32")
    if len(set(args.concurrency)) != len(args.concurrency):
        parser.error("concurrency values must be unique")
    counts = args.pdfium_processes or [None]
    if args.pdfium_processes and (any(not 1 <= count <= 32 for count in counts) or len(set(counts)) != len(counts)):  # pyright: ignore[reportOperatorIssue, reportOptionalOperand]
        parser.error("PDFium process counts must be unique and between 1 and 32")
    binary, config, pdfs = args.binary.resolve(), args.config.resolve(), args.pdf_dir.resolve()
    if not binary.is_file() or not config.is_file() or not pdfs.is_dir():
        parser.error("binary, configuration, and PDF directory must exist")
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    cases = []
    for processes in counts:
        for concurrency in args.concurrency:
            status = run_case(binary, config, pdfs, output, concurrency, args.repeats, processes)
            cases.append({"pdfium_processes": processes, "concurrency": concurrency, "exit_code": status})
            (output / "cases.json").write_text(json.dumps(cases, indent=2) + "\n")
    if any(case["exit_code"] != 0 for case in cases):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
