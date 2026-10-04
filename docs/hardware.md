# Hardware

How the app decides what your machine can run, and why it says what it says.

## What is measured

`smollm-hardware` reads the machine once at launch with `sysinfo` (and a little
platform probing), then caches it. The Home page has a **Re-measure** button
because free RAM changes while you work.

| Field | Source |
| --- | --- |
| `platform`, `arch` | `std::env::consts` |
| `cpuBrand`, `physicalCores`, `logicalCores` | `sysinfo::System` |
| `totalRamGb`, `availableRamGb` | `sysinfo` memory |
| `diskFreeGb`, `modelVolumeFreeGb` | statvfs on the data volume |
| `appleSilicon`, `metalAvailable` | CPU brand + `arch` (`aarch64` on macOS); Metal is assumed on any Mac |
| `nvidiaGpu` | `nvidia-smi --query-gpu=name` when it is on PATH |
| `vulkanAvailable` | `vulkaninfo --summary` succeeds |
| `gpuName` | Best available description, e.g. `Apple M3 Pro` |

Nothing is uploaded, and no measurement leaves the process.

## The RAM estimate

Loading a GGUF file needs more than the file size: weights, plus a KV cache that
grows with context, plus runtime overhead.

```
weights_gb   = sizeMb / 1000
kv_cache_gb  = contextLength × 0.00012
ram_needed   = weights_gb + kv_cache_gb + 0.35
```

So a 491 MB model at 32K context asks for about 0.49 + 3.9 + 0.35 ≈ 4.7 GB. That
is why the app sometimes says a 500 MB file needs 5 GB, and why dropping the
context length is the cheapest way to fit a bigger model.

Only 70% of *currently free* RAM counts as usable:

```
usable_ram_gb = max(availableRamGb × 0.7, 0)
fits          = ram_needed ≤ usable_ram_gb
```

The headroom exists because macOS and Windows will start swapping at exactly the
point where the arithmetic says it fits, and a swapping laptop is a bad
demonstration of a small model.

## Recommendations and bands

`build_doctor()` walks the catalog, keeps what fits, and reports the largest
parameter band that does — capped at 4B, because this app is about small models
and it would be dishonest to imply that a 32 GB machine should run 40B through a
UI built for laptops.

| Band | Headline you get |
| --- | --- |
| ≥ 3B | "Your machine looks great for 0.5B–4B models." |
| ≥ 1.7B | "Your machine can comfortably run 0.5B–1.7B models." |
| below that | "N GB RAM detected. Recommended: 0.5B and 1B models with Q4_K_M quantization." |

The detail line adapts to the accelerator it found: Apple Silicon unified memory,
an NVIDIA GPU (where CUDA beats CPU on 2B–3B), a Vulkan adapter, or nothing —
in which case it says CPU is the safe choice and that small models stay
responsive anyway.

## Warnings

The doctor adds warnings rather than refusing:

- Total RAM under 6 GB → close other apps, prefer 0.5B.
- Free RAM below 35% of total → free memory before loading something larger.
- Under 5 GB free on the model volume → downloads will start failing.
- Free RAM below 50% of total → a note appended to the detail line.

## Backend choice

```
macOS + Metal available        → metal
Windows/Linux + NVIDIA GPU     → cuda
Windows/Linux + Vulkan loader  → vulkan
otherwise                      → cpu
```

The chosen backend is a *recommendation*: it becomes the default in Settings, and
you can override it there. If a requested backend is not compiled into the build,
`EngineManager` falls back to something that works and logs the substitution,
which the UI surfaces as a warning toast instead of hiding.

## Practical guidance

| You have | Do this |
| --- | --- |
| 8 GB laptop | 0.5B–1.5B at Q4_K_M, context 4K–8K |
| 16 GB | 3B fits; keep context under 16K |
| 32 GB+ | 4B is still the recommendation; longer contexts become affordable |
| Intel Mac | CPU inference; stay at or below 1.5B |
| Windows + GTX/RTX | CUDA once a native backend is linked; CPU today |
| Only integrated graphics | CPU is fine for this size class |
