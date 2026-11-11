# vivid_gpu_share

Optional native GPU-frame constituent of `vivid_sdk`. The macOS-only prototype
copies completed WGPU textures on the GPU into bounded immutable IOSurfaces and
retains them through SDK GUI sampling. Native handles remain private.

The `gui` feature adds painting into the migrated GPUI runtime. Default builds
create no GUI or event loop. Other targets expose no sharing implementation.

See [the SDK contract](../docs/GUI-GPU-SHARING.md) for supported formats, limits,
threading, completion, lifetime, verification and remaining production work.
