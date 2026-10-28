- **Transparent windows and overlays stay clear on Windows, on every GPU.** A surface whose
  adapter can't composite alpha (a Win32 Vulkan surface on AMD's integrated GPUs, any DXGI
  swapchain made from an HWND) fell back to opaque, so an overlay drew a black box around its
  content, silently. DX12 surfaces now present through DirectComposition, which composites alpha
  on any GPU. A translucent surface gets the lowest-power adapter that can composite it, and
  reuses a shared device only if that device can. An overlay that still can't be translucent
  logs it, as a window already did.
