# ADR-0006: `WgpuDx12Bridge::copy_frame` — GPU-ordered copy via a shared cross-API fence

- **Status**: Accepted — compile-verified only (`cargo clippy --target x86_64-pc-windows-msvc`), **not hardware-verified**
- **Date**: 2026-11-17
- **Deciders**: @dev-nyxie (+ agent)
- **Crate**: `mediaway` (`wgpu` module) + `mediaway-encoder` (`windows::D3d12SharedEncodeBridge`)

## Context

`copy_frame` (ADR-0001) submits a `copy_texture_to_texture` into the bridge's shared D3D12
texture, then blocks the CPU on `device.poll(Wait)` for that submission, because the shared NT
handle carries no cross-device ordering: WMF's D3D11 device could otherwise read the texture
before the copy lands. That is one CPU↔GPU round-trip per frame.

`wgpu-hal` 30's DX12 `Queue` exposes `add_wait_fence` / `add_signal_fence`, which stage
`ID3D12CommandQueue::Wait`/`Signal` around the next `wgpu::Queue::submit`. D3D11.4
(`ID3D11Device5::OpenSharedFence`, `ID3D11DeviceContext4::Wait`/`Signal`) can open a
`D3D12_FENCE_FLAG_SHARED` fence. Together they allow GPU-side ordering with no CPU wait.

## Decision

> `D3d12SharedEncodeBridge` creates, best-effort at open, one shared fence opened on both APIs.
> `copy_frame` uses it when present and falls back to the old CPU stall otherwise.

Per frame (fence values strictly increase; one fence serves both directions):

1. `begin_d3d12_write`: D3D11 immediate context `Signal(v)` + `Flush` — after the encoder's
   previously submitted read of the texture (write-after-read hazard on the single-buffered
   destination).
2. wgpu: stage `Wait(v)` + `Signal(v+1)` on the hal queue, `submit` the copy.
3. `end_d3d12_write`: D3D11 immediate context `Wait(v+1)` — encoder work submitted after this
   reads only once the copy completed.

No CPU blocking on either side.

## Alternatives Considered

| Alternative | Why not |
|-------------|---------|
| Keep `poll(Wait)` | The per-frame CPU stall this ADR removes. |
| Keyed mutex (`IDXGIKeyedMutex`) | D3D12 resources do not support keyed mutexes. |
| `poll(Wait)` only every N frames | Still racy without ordering on the frames in between. |
| Apply the same to `handle()` (render-direct path) | Needs an empty submit to carry the signal; `wgpu-core` empty-submit behaviour not verified — left as is. |

## Consequences

### Positive

- No CPU↔GPU round-trip per frame on runtimes with `ID3D11Device5` (Windows 10 1703+).

### Negative / Trade-offs

- **Unverified assumption:** the WMF encoder MFT submits its texture reads through the
  device's immediate context. An MFT using a private/deferred context would not be ordered.
- One D3D11 `Flush` per frame remains; GPU-side waits still serialise copy → encode.
- `queue` must not be submitted to from another thread between staging and submit (staged
  wait/signal attach to the next submit).
- `handle()` and `WgpuDx12DecodeBridge` still use CPU stalls.

## References

- [ADR-0001](0001-dx12-hal-gpucopy-bridge.md), [ADR-0005](0005-render-target-and-external-shared-resource.md)
- `mediaway-encoder` [ADR windows/0006](../../../mediaway-encoder/adr/windows/0006-d3d12-shared-to-d3d11.md)
- `docs/spec/gpu-interop.md`, `docs/spec/caveats-and-clarity.md`
