# Zero-Copy

| Doc | Summary |
|-----|---------|
| [marks](marks.md) | README **⚡** = GPU **or** shared CPU (no payload memcpy) |
| [handles](handles.md) | `GpuBufferHandle` + CPU shared-buffer notes |
| [gpu-interop](gpu-interop.md) | wgpu / WebGPU / Dawn adapters (GPU ZC) |
| [sync-and-depth](sync-and-depth.md) | Cross-API fences instead of CPU stalls · `in_flight` depth · readback copy counts |
| Caveats | Costly copies/readbacks — [caveats-clarity](../meta/caveats-clarity.md) |
