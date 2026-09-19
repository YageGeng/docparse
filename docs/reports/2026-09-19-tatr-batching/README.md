# TATR batching consistency investigation

## Finding

Mixed-aspect-ratio batching is not structure-invariant. On a deliberately tall
400 × 800 distortion of the real table fixture, Rust singleton inference returns
126 cells, whereas batching it with a wider crop returns 96 cells. Both CPU and
CUDA reproduce this result at batch sizes 2, 4 and 8. This is a consistency stress
case, not a human-annotated accuracy measurement; neither count is asserted to be
correct for the distorted image.

Homogeneous batches of the original and tall fixtures retain singleton topology
at the tested sizes. The original table retains its 110 cells in mixed batches;
its maximum coordinate drift is about 1.2 pixels. The wide distortion and square
white-padded crop also retain topology. Identical behavior with reversed request
order, plus singleton/mixed parity with PyTorch, points to padding sensitivity,
not crossed replies or an ONNX export defect.

## Coverage and results

Physical Rust session calls used batch sizes 1, 2, 4 and 8, each with forward and
reverse sample orders, and homogeneous/mixed inputs. Each provider produced 60
sample observations. For each provider, all 30 homogeneous observations retained
topology; 7 of the 30 mixed observations changed topology, all corresponding to
the same tall stress image. Repeated observations are not independent documents.
The other inputs are the real 785 × 427 fixture, a 1200 × 200 distortion, and the
original placed on a 900 × 900 white canvas.

The existing CUDA queue integration check also passed with structure/cell batch
caps 1/1, 4/2 and 2/4, multiple consumers, five concurrent requests and different
image scales. It confirms returned request results, but does not assert that the
scheduler actually forms every possible full/tail batch. The physical-session
matrix explicitly checks batch size and output count.

A separate CPU PyTorch/ONNX check used a bilinear tall distortion:

- Homogeneous `[2,3,800,400]`: maximum confident-box drift from singleton in
  PyTorch was approximately 1.19e-7 normalized units.
- Mixed `[2,3,800,800]`: the same PyTorch sample drifted by approximately 0.0204
  normalized units.
- ONNX/PyTorch parity passed in singleton, homogeneous and mixed cases with
  `rtol=1e-3, atol=1e-3`; maximum observed logit error was 5.25e-5 and box error
  was 8.05e-6.

Thus the padding effect exists before the Rust postprocessor. The current
postprocessor can turn those prediction changes into different merged-cell
structures. This does not establish that every mixed table fails, nor certify
batch sizes 16/32 or browser WebGPU execution. The GPU was an RTX 4060 Laptop;
this was a correctness investigation under an active local service, not a
throughput benchmark.

## Operational conclusion

Keep `tsr.batch_size = 1` when singleton-equivalent behavior is required. The
local configuration currently already uses 1. Existing `session_size` concurrency
and shared cell detection remain available. No production code or configuration
was changed during this investigation.

Potential follow-up remedies are grouping identical tensor shapes, or consistently
padding both singleton and batch inference to a fixed shape. Grouping requires
queue scheduling work; fixed padding changes the singleton baseline and requires
new accuracy/performance evaluation. Neither is implemented here.

## Reproduce

```sh
rtk cargo test -p docparse-tsr --lib real_batch_matrix -- --ignored --nocapture
rtk cargo test -p docparse-tsr --features cuda --lib real_batch_matrix -- --ignored --nocapture
rtk cargo test -p docparse-tsr --features cuda --test inference configurable_batches_match_singleton_predictions -- --ignored
rtk proxy /tmp/docparse-tatr-env/bin/python crates/tsr/tests/python/tatr_batch_parity.py
```

Use the Python environment from the earlier TATR export report. The Rust matrix
is an empirical report generator: its passing status means inference and output
routing succeeded, not that every reported `same_topology` value is true. See
[measurements.json](measurements.json) for every observation.
