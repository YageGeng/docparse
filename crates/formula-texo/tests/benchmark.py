"""Compare Texo CPU/CUDA encoder and cached greedy decoding on the same real fixtures."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import time

import numpy as np
import onnxruntime as ort

from reference import preprocess


class Runner:
    """Match native standard runs with host-resident hidden states and caches."""

    def __init__(self, directory, provider, threads):
        """Match the service's All optimization and memory-pattern settings for each provider."""
        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        options.enable_mem_pattern = True
        self.encoder = ort.InferenceSession(str(directory / "encoder_model.onnx"), options, providers=[provider])
        self.decoder = ort.InferenceSession(str(directory / "decoder_model_merged.onnx"), options, providers=[provider])
        assert self.encoder.get_providers()[0] == provider
        assert self.decoder.get_providers()[0] == provider

    def run(self, pixels):
        """Measure synchronous inference, token selection and cache management, excluding image preparation."""
        batch = len(pixels)
        total_started = time.perf_counter()
        started = time.perf_counter()
        hidden = self.encoder.run(["last_hidden_state"], {"pixel_values": pixels})[0]
        encoder_ms = (time.perf_counter()-started)*1000
        cache = {value.name: np.zeros((batch,16,0,24),dtype=np.float32)
                 for value in self.decoder.get_inputs() if value.name.startswith("past_key_values")}
        names = [value.name for value in self.decoder.get_outputs()]
        tokens = [[0] for _ in range(batch)]
        done = np.zeros(batch, dtype=bool)
        next_ids = np.zeros((batch,1),dtype=np.int64)
        decoder_ms = 0.0
        for step in range(1023):
            started = time.perf_counter()
            inputs = {"input_ids": next_ids, "use_cache_branch": np.array([step > 0]),
                      "encoder_hidden_states": hidden, **cache}
            outputs = dict(zip(names, self.decoder.run(names, inputs)))
            decoder_ms += (time.perf_counter()-started)*1000
            best = outputs["logits"][:, -1].argmax(-1)
            next_ids = best[:,None].astype(np.int64)
            for index, token in enumerate(best):
                if done[index]:
                    next_ids[index,0] = 1
                else:
                    tokens[index].append(int(token))
                    done[index] = token == 2
            if done.all():
                break
            for name in cache:
                if step == 0 or ".decoder." in name:
                    cache[name] = outputs[name.replace("past_key_values", "present")]
        assert done.all(), "decoder did not reach EOS"
        return {"total_ms":(time.perf_counter()-total_started)*1000,"encoder_ms":encoder_ms,
                "decoder_ms":decoder_ms,"steps":step+1,"tokens":tokens}


def main():
    """Warm each input shape and report repeated measurements with exact golden-token checks."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-dir",type=Path,default=Path("models/texo"))
    parser.add_argument("--provider",choices=["cpu","cuda"],required=True)
    parser.add_argument("--threads",type=int,default=1)
    parser.add_argument("--repeats",type=int,default=5)
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    if args.threads < 1 or args.repeats < 1: parser.error("threads and repeats must be positive")
    fixtures=Path(__file__).parent/"fixtures"
    golden={case["image"]:case["tokens"] for case in json.loads((fixtures/"reference.json").read_text())["cases"]}
    pixels={name:preprocess(fixtures/name) for name in golden}
    provider="CPUExecutionProvider" if args.provider=="cpu" else "CUDAExecutionProvider"
    runner=Runner(args.model_dir,provider,args.threads)
    groups=[[name] for name in golden]
    groups.append(["formula_single.png","formula_single2.png","formula_multi.png","formula_single.png"])
    report={"host":platform.node(),"runtime":ort.__version__,"provider":provider,"threads":args.threads,
            "cache_device":"cpu","execution":"standard_run","optimization":"all","memory_pattern":True,"cases":[],
            "model_sha256":{name:hashlib.sha256((args.model_dir/name).read_bytes()).hexdigest()
                            for name in ["encoder_model.onnx","decoder_model_merged.onnx"]}}
    for names in groups:
        tensor=np.concatenate([pixels[name] for name in names])
        expected=[golden[name] for name in names]
        warmup=runner.run(tensor)
        assert warmup["tokens"] == expected, f"warmup token mismatch: {names}"
        samples=[]
        for _ in range(args.repeats):
            result=runner.run(tensor)
            assert result.pop("tokens") == expected, f"token mismatch: {names}"
            samples.append(result)
        case={"images":names,"batch":len(names),"steps":samples[0]["steps"],"exact_tokens":True,"samples":samples}
        for key in ["total_ms","encoder_ms","decoder_ms"]:
            case[key]=float(np.median([s[key] for s in samples]))
        report["cases"].append(case)
        print(json.dumps({k:v for k,v in case.items() if k != "samples"}),flush=True)
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_text(json.dumps(report,indent=2)+"\n")


if __name__ == "__main__":
    main()
