//! D3D12 shared heap → native D3D11 texture for WMF encode (storm-chaser pattern).
//!
//! **Path class: `GpuCopy`** — callers typically `CopyResource` into the shared D3D12
//! texture once per frame, then submit the opened [`ID3D11Texture2D`] to the encoder.
//! Do **not** use `D3D11On12` for NVENC/MF — that path yields `MF_E_UNSUPPORTED_D3D_TYPE`.
//!
//! See [ADR-0006](../adr/0006-d3d12-shared-to-d3d11.md).
//!
//! **Cross-API fence (optional, best-effort at open):** when the native D3D11 device exposes
//! `ID3D11Device5` (Windows 10 1703+), the bridge also creates one `D3D12_FENCE_FLAG_SHARED`
//! fence opened on both APIs, so a D3D12 producer can order its copy against the D3D11 encoder
//! on the GPU (see [`D3d12SharedEncodeBridge::begin_d3d12_write`]) instead of a CPU stall.
//! **Not hardware-verified** — compiled (`cargo check`, `x86_64-pc-windows-msvc`) only.

#![allow(unsafe_code)]

use crate::EncodeError;
use mediaway_common::{GpuBufferHandle, NativeHandle};
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_1;

use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext4,
    ID3D11Fence, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED,
    D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_MEMORY_POOL_UNKNOWN, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
    D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Device, ID3D12Fence,
    ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory2, IDXGIAdapter, IDXGIFactory4};
use windows::core::{Interface, PCWSTR};

/// Shared D3D12 texture opened as a native D3D11 texture for MF/DXGI encode.
pub struct D3d12SharedEncodeBridge {
    d3d12_resource: ID3D12Resource,
    d3d11_device: ID3D11Device,
    d3d11_texture: ID3D11Texture2D,
    shared_handle: HANDLE,
    /// `None` when the D3D11 device lacks `ID3D11Device5` or fence sharing failed —
    /// callers then fall back to a CPU-side wait.
    fence: Option<CrossApiFence>,
}

/// One shared fence opened on both APIs plus the D3D11 immediate context that signals/waits on
/// it. Values only ever increase; each side waits for the previous value before signalling the
/// next, so one fence serves both directions (D3D12 write -> D3D11 read -> next D3D12 write).
struct CrossApiFence {
    d3d12: ID3D12Fence,
    d3d11: ID3D11Fence,
    context: ID3D11DeviceContext4,
    last: AtomicU64,
}

/// Fence values for one D3D12-side write of the shared texture, from
/// [`D3d12SharedEncodeBridge::begin_d3d12_write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3d12WriteFence {
    /// The D3D12 queue must GPU-wait for this value before writing (D3D11 finished reading the
    /// previous frame).
    pub wait_value: u64,
    /// The D3D12 queue must signal this value after its write; pass it to
    /// [`D3d12SharedEncodeBridge::end_d3d12_write`].
    pub signal_value: u64,
}

impl D3d12SharedEncodeBridge {
    /// Allocate a `D3D12_HEAP_FLAG_SHARED` BGRA texture on `d3d12_device` and open it
    /// on a **native** same-adapter D3D11 device via `OpenSharedResource1`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] for zero size, or
    /// [`EncodeError::Backend`] on D3D / DXGI failure.
    pub fn open(d3d12_device: NativeHandle, width: u32, height: u32) -> Result<Self, EncodeError> {
        if width == 0 || height == 0 {
            return Err(EncodeError::InvalidInput);
        }
        let d3d12 = device_from_handle(d3d12_device)?;

        let heap_props = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            // ALLOW_RENDER_TARGET required — bare shared heaps fail OpenSharedResource1.
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: CreateCommittedResource with SHARED heap for cross-API open.
        unsafe {
            d3d12
                .CreateCommittedResource(
                    &raw const heap_props,
                    D3D12_HEAP_FLAG_SHARED,
                    &raw const desc,
                    D3D12_RESOURCE_STATE_COMMON,
                    None,
                    &raw mut resource,
                )
                .map_err(|_| EncodeError::Backend)?;
        }
        let resource = resource.ok_or(EncodeError::Backend)?;

        Self::from_resource(&d3d12, resource)
    }

    /// Share a caller-owned `ID3D12Resource*` instead of allocating one — `resource` must
    /// already be `D3D12_HEAP_FLAG_SHARED`-allocated (`ALLOW_RENDER_TARGET` recommended for
    /// render-target use) on `d3d12_device`. No `width`/`height` parameter: unlike [`Self::open`],
    /// this does not allocate, so the resource's own dimensions apply.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] for a null device/resource pointer, or
    /// [`EncodeError::Backend`] on D3D / DXGI failure — including a `resource` that was not
    /// actually shared-heap-allocated (`CreateSharedHandle` itself fails for a non-shared
    /// resource).
    pub fn open_with_resource(
        d3d12_device: NativeHandle,
        d3d12_resource: NativeHandle,
    ) -> Result<Self, EncodeError> {
        let d3d12 = device_from_handle(d3d12_device)?;
        let raw = d3d12_resource.get() as *mut std::ffi::c_void;
        // SAFETY: caller guarantees a live, D3D12_HEAP_FLAG_SHARED-allocated ID3D12Resource*.
        let borrowed =
            unsafe { ID3D12Resource::from_raw_borrowed(&raw) }.ok_or(EncodeError::InvalidInput)?;
        // clone: COM AddRef so we own a reference for CreateSharedHandle/the struct's lifetime
        let resource: ID3D12Resource = borrowed.clone();

        Self::from_resource(&d3d12, resource)
    }

    /// Shared tail of [`Self::open`]/[`Self::open_with_resource`]: `CreateSharedHandle` on
    /// `resource`, then open it on a **native** same-adapter D3D11 device via
    /// `OpenSharedResource1`.
    fn from_resource(d3d12: &ID3D12Device, resource: ID3D12Resource) -> Result<Self, EncodeError> {
        // SAFETY: CreateSharedHandle for NT handle open on D3D11.
        let shared_handle: HANDLE = unsafe {
            d3d12
                .CreateSharedHandle(&resource, None, GENERIC_ALL.0, PCWSTR::null())
                .map_err(|_| EncodeError::Backend)?
        };

        let adapter_luid = unsafe { d3d12.GetAdapterLuid() };
        let factory: IDXGIFactory4 = unsafe {
            CreateDXGIFactory2(windows::Win32::Graphics::Dxgi::DXGI_CREATE_FACTORY_FLAGS(0))
        }
        .map_err(|_| EncodeError::Backend)?;
        let adapter: IDXGIAdapter =
            unsafe { factory.EnumAdapterByLuid(adapter_luid) }.map_err(|_| EncodeError::Backend)?;

        let mut d3d11_device: Option<ID3D11Device> = None;
        let mut feature_level = D3D_FEATURE_LEVEL::default();
        // SAFETY: same-adapter native D3D11 (not D3D11On12).
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&raw mut d3d11_device),
                Some(&raw mut feature_level),
                None,
            )
        }
        .map_err(|_| EncodeError::Backend)?;
        let _ = feature_level;
        let d3d11_device = d3d11_device.ok_or(EncodeError::Backend)?;
        let device1: ID3D11Device1 = d3d11_device.cast().map_err(|_| EncodeError::Backend)?;
        // SAFETY: OpenSharedResource1 on native D3D11Device1.
        let opened: ID3D11Resource = unsafe { device1.OpenSharedResource1(shared_handle) }
            .map_err(|_| EncodeError::Backend)?;
        let d3d11_texture: ID3D11Texture2D = opened.cast().map_err(|_| EncodeError::Backend)?;
        let fence = open_cross_api_fence(d3d12, &d3d11_device);

        Ok(Self {
            d3d12_resource: resource,
            d3d11_device,
            d3d11_texture,
            shared_handle,
            fence,
        })
    }

    /// Opaque `ID3D12Fence*` of the bridge's shared cross-API fence, or `None` when the D3D11
    /// device could not open a shared fence (pre-`ID3D11Device5` runtime / driver) — callers
    /// must then fall back to a CPU wait before pushing the frame.
    ///
    /// # Errors
    ///
    /// See [`Self::d3d12_resource_handle`].
    pub fn d3d12_fence_handle(&self) -> Result<Option<NativeHandle>, EncodeError> {
        self.fence
            .as_ref()
            .map(|f| to_native_handle(&f.d3d12))
            .transpose()
    }

    /// Start one D3D12-side write of the shared texture with GPU-side ordering only (no CPU
    /// wait). Enqueues `Signal(fence, wait_value)` on the D3D11 immediate context (after any
    /// encoder work already submitted there, i.e. the previous frame's read) and flushes it.
    /// The caller must then make its D3D12 queue `Wait(wait_value)` before the write and
    /// `Signal(signal_value)` after it, then call [`Self::end_d3d12_write`].
    ///
    /// Returns `Ok(None)` when no shared fence is available (see [`Self::d3d12_fence_handle`]).
    ///
    /// **Cost:** one D3D11 `Flush` per frame (a CPU-side command-buffer submit, not a GPU
    /// wait). **Assumption (not hardware-verified):** the WMF encoder MFT submits its reads of
    /// the texture through this device's immediate context; an MFT that used a private context
    /// would not be ordered by this fence.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Backend`] if the D3D11 `Signal` call fails.
    pub fn begin_d3d12_write(&self) -> Result<Option<D3d12WriteFence>, EncodeError> {
        let Some(fence) = &self.fence else {
            return Ok(None);
        };
        let wait_value = fence.last.fetch_add(2, Ordering::AcqRel) + 1;
        // SAFETY: `fence.d3d11` and `fence.context` are live COM interfaces owned by `self`.
        unsafe {
            fence
                .context
                .Signal(&fence.d3d11, wait_value)
                .map_err(|_| EncodeError::Backend)?;
            fence.context.Flush();
        }
        Ok(Some(D3d12WriteFence {
            wait_value,
            signal_value: wait_value + 1,
        }))
    }

    /// Finish a write begun with [`Self::begin_d3d12_write`]: enqueue a GPU-side
    /// `Wait(fence, signal_value)` on the D3D11 immediate context, so encoder work submitted
    /// after this call reads the texture only once the D3D12 write has completed. Never blocks
    /// the CPU.
    ///
    /// # Errors
    ///
    /// [`EncodeError::InvalidInput`] when the bridge has no shared fence;
    /// [`EncodeError::Backend`] if the D3D11 `Wait` call fails.
    pub fn end_d3d12_write(&self, write: D3d12WriteFence) -> Result<(), EncodeError> {
        let fence = self.fence.as_ref().ok_or(EncodeError::InvalidInput)?;
        // SAFETY: `fence.d3d11` and `fence.context` are live COM interfaces owned by `self`.
        unsafe { fence.context.Wait(&fence.d3d11, write.signal_value) }
            .map_err(|_| EncodeError::Backend)
    }

    /// Opaque `ID3D12Resource*` for caller `CopyResource` into the shared heap.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] if the live COM interface somehow yields a
    /// null pointer (not expected in practice — a valid `Interface` value is never
    /// backed by a null vtable).
    pub fn d3d12_resource_handle(&self) -> Result<NativeHandle, EncodeError> {
        to_native_handle(&self.d3d12_resource)
    }

    /// Opaque `ID3D11Device*` (use as encoder `gpu_device`).
    ///
    /// # Errors
    ///
    /// See [`Self::d3d12_resource_handle`].
    pub fn d3d11_device_handle(&self) -> Result<NativeHandle, EncodeError> {
        to_native_handle(&self.d3d11_device)
    }

    /// Opaque `ID3D11Texture2D*` for [`GpuBufferHandle::DirectX11`] encode push.
    ///
    /// # Errors
    ///
    /// See [`Self::d3d12_resource_handle`].
    pub fn d3d11_texture_handle(&self) -> Result<NativeHandle, EncodeError> {
        to_native_handle(&self.d3d11_texture)
    }

    /// Shared NT handle as [`GpuBufferHandle::DirectXShared`].
    ///
    /// # Errors
    ///
    /// See [`Self::d3d12_resource_handle`].
    pub fn shared_handle(&self) -> Result<GpuBufferHandle, EncodeError> {
        let handle =
            NativeHandle::new(self.shared_handle.0 as usize).ok_or(EncodeError::Backend)?;
        Ok(GpuBufferHandle::DirectXShared { handle })
    }

    /// Encode-ready `DirectX11` handle (subresource 0).
    ///
    /// # Errors
    ///
    /// See [`Self::d3d12_resource_handle`].
    pub fn as_dx11_handle(&self) -> Result<GpuBufferHandle, EncodeError> {
        Ok(GpuBufferHandle::DirectX11 {
            texture: self.d3d11_texture_handle()?,
            subresource: 0,
        })
    }
}

/// Borrow + clone (COM `AddRef`) a caller-supplied `ID3D12Device*` handle.
fn device_from_handle(d3d12_device: NativeHandle) -> Result<ID3D12Device, EncodeError> {
    let raw = d3d12_device.get() as *mut std::ffi::c_void;
    // SAFETY: caller guarantees a live ID3D12Device* for the session.
    let borrowed =
        unsafe { ID3D12Device::from_raw_borrowed(&raw) }.ok_or(EncodeError::InvalidInput)?;
    Ok(borrowed.clone()) // clone: COM AddRef so we own the device for this call's lifetime
}

/// Best-effort shared fence creation: any failure (no `ID3D11Device5`, driver without shared
/// fences) yields `None` and the caller keeps the CPU-wait fallback.
fn open_cross_api_fence(d3d12: &ID3D12Device, d3d11: &ID3D11Device) -> Option<CrossApiFence> {
    let device5: ID3D11Device5 = d3d11.cast().ok()?;
    // SAFETY: plain fence creation on a live device.
    let fence12: ID3D12Fence = unsafe { d3d12.CreateFence(0, D3D12_FENCE_FLAG_SHARED) }.ok()?;
    // SAFETY: `fence12` was created with `D3D12_FENCE_FLAG_SHARED`.
    let handle =
        unsafe { d3d12.CreateSharedHandle(&fence12, None, GENERIC_ALL.0, PCWSTR::null()) }.ok()?;
    let mut fence11: Option<ID3D11Fence> = None;
    // SAFETY: `handle` is the NT handle just created for `fence12` on the same adapter.
    let opened = unsafe { device5.OpenSharedFence(handle, &raw mut fence11) };
    // SAFETY: the opened fence holds its own reference; our NT handle is no longer needed.
    let _ = unsafe { CloseHandle(handle) };
    opened.ok()?;
    // SAFETY: `GetImmediateContext` on a live device.
    let context: ID3D11DeviceContext4 = unsafe { d3d11.GetImmediateContext() }.ok()?.cast().ok()?;
    Some(CrossApiFence {
        d3d12: fence12,
        d3d11: fence11?,
        context,
        last: AtomicU64::new(0),
    })
}

/// Wrap a live COM interface's raw pointer as a [`NativeHandle`].
///
/// A `windows`-crate [`Interface`] value that exists at all is never backed by a
/// null vtable pointer — this returns `Result` instead of panicking so a
/// theoretical violation surfaces as [`EncodeError::Backend`], never a panic.
fn to_native_handle<T: Interface>(obj: &T) -> Result<NativeHandle, EncodeError> {
    NativeHandle::new(Interface::as_raw(obj) as usize).ok_or(EncodeError::Backend)
}

impl Drop for D3d12SharedEncodeBridge {
    fn drop(&mut self) {
        if !self.shared_handle.is_invalid() {
            let _ = unsafe { CloseHandle(self.shared_handle) };
        }
    }
}
