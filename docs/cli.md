# Using the command line

The command line has six commands: `list`, `show`, `run`, `about`, `serve` and `doctor`. Each one has `--help`.

## Finding filters

8bf-host looks for filters in folders, including their subfolders. On Windows and macOS the usual Adobe plug-in folders are added automatically when they exist, as is `~/8bf-plugins` everywhere. You can add folders three ways: `--plugin-dir` on the command line, the `EIGHTBF_HOST_PLUGIN_PATH` environment variable (separated like `PATH`), or `plugin_dirs` in the config file.

```console
$ 8bf-host --plugin-dir ~/filters list
443b91e33c0e  Wine/Error   under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
8c8224326f39  Wine/Invert  under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
3776e356a9f9  Wine/Suites  under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
```

Each line has three parts. The first column is the filter's **id**, a short hash of its file and name that stays the same between runs. Next is the filter's category and name. The last column says where the filter would run, or why it cannot run here. Scan results are cached, so later runs only re-read plug-in files whose size or modification time changed; `--rescan` ignores the cache.

Commands that take a filter accept any of four forms: the id, `Category/Name`, the name alone (if it is unique), or the path to a plug-in file. Names are matched without regard to case. If a name is ambiguous, the error lists the candidates with their ids.

`show` describes one filter:

```console
$ 8bf-host show Wine/Invert
Invert
  id:         8c8224326f39
  category:   Wine
  file:       /home/you/filters/testfilter.8bf
  code:       win-x64
  modes:      rgb8, gray8
  about box:  yes
  runs:       under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
```

`modes` lists the image types the filter declares support for. This one only supports 8-bit images, so a 16-bit input would run on an 8-bit copy, as described in [Compatibility](compatibility.md#what-works).

## Running a filter

The usage is:

```text
8bf-host run [OPTIONS] <FILTER> <INPUT> <OUTPUT>
```

The simplest form runs the filter with its default settings, without its dialog:

```console
$ 8bf-host run Wine/Invert photo.png inverted.png
```

The output format comes from the output file's extension, or `--format`. The output keeps the input's layout: an RGBA PNG stays RGBA, and 16 bits stay 16 bits where the format allows.

Either file can be `-` for standard input or output, which lets 8bf-host sit in a pipeline. With standard output there is no extension to read, so name the format:

```console
$ curl -s https://example.com/photo.png | 8bf-host run 8c8224326f39 - - --format png > out.png
```

To filter part of an image, pass a rectangle as `X,Y,WIDTH,HEIGHT`, or a mask image the same size as the input, where white is selected and grey is partly selected:

```console
$ 8bf-host run Wine/Invert photo.png out.png --rect 100,50,400,300
$ 8bf-host run Wine/Invert photo.png out.png --mask selection.png
```

Some filters use the foreground and background colors or the image resolution; `--foreground`, `--background` (as `#rrggbb`) and `--resolution` set them.

## Settings and presets

Most filters have a dialog, and the settings you choose there are what makes a filter useful. 8bf-host handles this in two steps. First, run the filter once with `--dialog` and save its settings to a preset:

```console
$ 8bf-host run "Acme/Unsharp Mask" sample.png check.png --dialog --save-preset unsharp.json
```

Then replay the preset without the dialog, as many times as you like:

```console
$ for f in scans/*.tif; do
>   8bf-host run "Acme/Unsharp Mask" "$f" "sharp/$(basename "$f")" --preset unsharp.json
> done
```

A preset holds two things: the filter's own parameter block, which is opaque binary data, and its **descriptor**, the named settings that scripting-aware filters record for Photoshop actions. The descriptor is readable JSON, so you can adjust a setting without opening the dialog again:

```json
{
  "format": "8bf-host-preset",
  "version": 1,
  "filter": { "name": "Unsharp Mask", "category": "Acme", "id": "1a2b3c4d5e6f" },
  "parameters": "NBIAAAEAAAA=",
  "descriptor": [
    { "key": "Amnt", "type": "long", "value": 7 }
  ]
}
```

Whether editing works depends on the filter. Filters that use descriptors read them when replaying, so a changed `value` takes effect. Filters that do not use them only read the opaque `parameters`.

## Repeated runs

`--repeat N` runs the filter N times in one process, each run on the last one's output, the way Photoshop's Last Filter command (Ctrl+F) reapplies a filter: the dialog shows at most once, and later runs reuse the settings the filter kept. The output is the last run's image.

Photoshop also unloads a filter's code after every run and loads it again for the next. `--unload-between-runs` does the same, which catches filters that leave code running after they return, such as a thread pool. On Windows the host then reserves the freed address range with no access, so such code faults instead of running whatever DLL lands there next, and it looks for threads that started inside the unloaded filter. Finding any fails the run, because in Photoshop they crash the application when they wake:

```console
$ 8bf-host run --run-mode in-process Acme/Halftone photo.png out.png --repeat 2 --unload-between-runs
[host] halftone.8bf unloaded with 16 of its threads still running in its freed code at 0x6ffffdff0000 (+0x22f000)
error: Halftone... left 16 threads running after it was unloaded; hosts that unload filters, as Photoshop does, crash when they wake
```

A filter that pins itself in memory reports that it is still mapped after unloading, and passes. Unloading needs the filter to run in this process, so pass `--run-mode in-process`; a worker process unloads everything when it exits anyway. `--pause MS` waits after each run, for code that misbehaves only some time after the filter returns.

## Exit codes

Scripts can tell failures apart by the exit code:

| Code | Meaning |
|------|---------|
| 0    | Success |
| 1    | The filter ran and reported an error |
| 2    | Invalid arguments |
| 3    | No filter matches the name, or several do |
| 4    | The filter cannot run on this system with the current configuration |
| 5    | An image could not be read or written |
| 6    | The configuration file is invalid |
| 130  | Cancelled (Ctrl+C, or the filter's own Cancel button) |

The first Ctrl+C asks the filter to stop, which well-behaved filters do at their next progress check. A second Ctrl+C exits at once. `--json` prints a summary of the run (status, where it ran, the host log) for scripts that want more than an exit code.
