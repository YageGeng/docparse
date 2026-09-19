# Texo CPU versus CUDA inference

## Method

Local: Intel Core i9-14900HX and RTX 4060 Laptop 8 GB, GPU idle before testing. Remote: Xeon Platinum 8352V and RTX 4080 SUPER 32 GB while the production Texo queue remained busy (16 consumers, 64 queued requests at the initial observation). The remote service was not stopped or reconfigured. These are single benchmark-session latency measurements, not a comparison of maximum multi-session throughput.

ONNX Runtime 1.29.0, graph optimization All, memory pattern enabled. One encoder/decoder pair, CPU intra-op threads 1 or 4; CUDA host intra-op threads 1 with provider defaults. CUDA hidden states and KV caches remain in device memory; only logits return to CPU for greedy selection. CPU uses host-bound outputs. The script uses synchronous I/O binding, including binding and output synchronization overhead in stage timings. Input preprocessing and session loading are outside the measured interval. Full model generation includes greedy selection and cache management. All measured and warmup token sequences exactly match the checked-in reference.

Each case has one warmup, followed by five local or three remote measured runs. Values below are medians, in milliseconds. Remote runs are sequential under changing production load and should not be read as isolated GPU capability. The mixed batch has four crops and waits for its longest, 433-step sequence.

## Local

| Case | CPU 1 thread | CPU 4 threads | CUDA |
| --- | ---: | ---: | ---: |
| Long formula (433 steps) | 405.6 | 201.2 | 185.1 |
| Short formula (52 steps) | 142.6 | 54.3 | 24.6 |
| Medium formula (134 steps) | 188.3 | 83.8 | 57.3 |
| Mixed batch of 4 (433 steps) | 1135.5 | 427.6 | 231.6 |

| Case | CPU4 encoder | CUDA encoder | CPU4 decoder | CUDA decoder |
| --- | ---: | ---: | ---: | ---: |
| Long formula (433 steps) | 34.5 | 3.5 | 160.1 | 174.7 |
| Short formula (52 steps) | 34.3 | 3.4 | 19.1 | 20.3 |
| Medium formula (134 steps) | 33.9 | 3.4 | 47.8 | 51.8 |
| Mixed batch of 4 (433 steps) | 133.8 | 13.2 | 286.1 | 210.3 |

## Remote

| Case | CPU 1 thread | CPU 4 threads | CUDA |
| --- | ---: | ---: | ---: |
| Long formula (433 steps) | 907.7 | 762.2 | 1097.0 |
| Short formula (52 steps) | 250.9 | 161.8 | 89.7 |
| Medium formula (134 steps) | 370.5 | 325.1 | 266.1 |
| Mixed batch of 4 (433 steps) | 2385.6 | 1018.2 | 958.8 |

| Case | CPU4 encoder | CUDA encoder | CPU4 decoder | CUDA decoder |
| --- | ---: | ---: | ---: | ---: |
| Long formula (433 steps) | 98.3 | 12.6 | 642.0 | 1042.5 |
| Short formula (52 steps) | 73.0 | 4.4 | 85.8 | 78.6 |
| Medium formula (134 steps) | 108.1 | 3.9 | 196.6 | 254.2 |
| Mixed batch of 4 (433 steps) | 241.8 | 8.8 | 747.4 | 926.5 |

## Interpretation

CUDA was faster end-to-end in all four idle local cases. Its encoder advantage is substantial, but cached decoder steps are small and sequential: local four-thread CPU decoding was slightly faster for the singleton fixtures. On the busy remote GPU, the long-formula case was slower than both CPU configurations. Short and medium cases still favored CUDA; the mixed batch was close to four-thread CPU. Three loaded samples do not justify claiming a stable six-percent GPU advantage for the mixed batch.

Do not infer that moving the entire production workload to CPU will improve throughput. The benchmark adds one session while sixteen existing CUDA consumers are already competing, and does not test sixteen CPU consumers. A follow-up concurrency sweep or separate encoder/decoder placement experiment is needed to select a production configuration. No backend settings were changed.

## Reproduce

Use a Python 3.12 environment with onnxruntime-gpu 1.29.0, NumPy and Pillow, and CUDA/cuDNN libraries for the GPU run. From the repository root:

```sh
python crates/formula-texo/tests/benchmark.py --provider cpu --threads 1 --output /tmp/texo-cpu1.json
python crates/formula-texo/tests/benchmark.py --provider cpu --threads 4 --output /tmp/texo-cpu4.json
python crates/formula-texo/tests/benchmark.py --provider cuda --output /tmp/texo-cuda.json
```

Run providers sequentially. Supply `--repeats 3` to reproduce the remote sample count. Raw timings, model hashes and output-equality evidence are retained in [measurements.json](measurements.json).
