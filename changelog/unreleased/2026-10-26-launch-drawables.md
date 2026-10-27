- **A new window holds two drawables until it rests, on macOS.** A window's Metal surface now
  starts at frame latency 1 (two drawables) and goes to its configured 2 (three) once the window
  first goes idle on screen, or after its first second of frames there. A window that drew frames
  back to back while coming on screen (an app animating at launch, or launch frames waiting on
  the window server or on shader compiles) made CAMetalLayer allocate a third window-sized
  drawable and keep it: about 12 MB more settled memory for a 980x760 window. Raising the count
  allocates nothing until a frame needs it, so a window that animates later still gets its third
  drawable then.
