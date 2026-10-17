# Compatibility

## What works

- Filters on 8- and 16-bit RGB and grayscale images, with or without alpha.
- Selections. You pass a mask image; the filter receives it, and the result is blended through it as Photoshop's "auto mask" does, including soft edges.
- Presets. A filter's settings can be saved after a run (with or without showing its dialog) and replayed later without the dialog, in batch or over HTTP.
- 16-bit images with filters that only declare 8-bit support. The host runs the filter on an 8-bit copy and keeps the full 16-bit data for every sample the filter left unchanged. You can turn this off.
- The callback suites filters use in practice: handles, buffers, resources, properties, color services, the descriptor (scripting) suite, and the PICA basic suite with the handle, buffer, UI-hooks and property suites. Previews through `displayPixels` work on Windows (GDI) and macOS (Core Graphics).
- Image files in PNG, JPEG, TIFF, WebP, BMP and PNM. PNG, TIFF and PNM keep 16 bits per channel.

## Known limitations

These are worth knowing about before you depend on 8bf-host:

- **Color modes.** Only RGB and grayscale are supported. CMYK and Lab images, and floating-point (HDR) files, need converting first.
- **Not implemented:** channel ports, image services (resampling callbacks), floating selections, absolute channel indexing, and layer masks. A filter that requires one of these reports an error, typically "requires features this host does not provide".
- **Metadata.** ICC profiles and EXIF data are not copied from the input to the output.
- **In-process crashes.** A filter running in-process shares the process with 8bf-host. If it crashes, the process goes with it. The server isolates every run in a worker process by default for this reason; the command line does not, since one crashed command only loses one run. Use `--run-mode worker` if you want isolation there too.
- **Dialogs need a desktop.** `--dialog` shows the filter's own window, so it needs a display. The server never shows dialogs.
- **Plug-in compatibility.** Some commercial filters check for Photoshop specifically, require activation through Adobe components, or rely on undocumented behavior. Expect some of them to refuse to run.
- **macOS is tested in CI only.** The test suite runs on a Mac in CI, but the macOS-specific parts (bundle loading, previews, Rosetta workers) have not been tried with real Adobe filters.
