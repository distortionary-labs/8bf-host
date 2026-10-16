# HTTP API reference

`8bf-host serve` exposes the installed filters over HTTP. This document lists every endpoint, option and error. For an introduction, and for the security defaults, see the [README](../README.md#using-the-http-server).

All paths start with `/v1`. Responses are JSON, except a successful run, which returns the image. The version prefix will change only for incompatible changes; new fields may appear in JSON responses at any time, so clients should ignore fields they do not know.

## Authentication

When a token is configured (`EIGHTBF_HOST_TOKEN` or `server.token`), every endpoint except `/v1/health` requires it:

```text
Authorization: Bearer <token>
```

A missing or wrong token gets `401` with a `WWW-Authenticate: Bearer` header. Without a configured token, the server only listens on loopback addresses unless started with `--insecure-no-auth`.

## Endpoints

### `GET /v1/health`

Liveness. Never needs a token.

```json
{"status": "ok", "version": "0.1.0"}
```

### `GET /v1/filters`

The installed filters, in category and name order.

```json
{
  "filters": [
    {
      "id": "8c8224326f39",
      "name": "Invert",
      "category": "Wine",
      "path": "/srv/filters/testfilter.8bf",
      "format": "pe",
      "archs": ["win-x64"],
      "modes": ["rgb8", "gray8"],
      "has_about_box": true,
      "runs": "under Wine (win-x64, /usr/local/lib/8bf-host/8bf-host-win-x64.exe)",
      "problem": null
    }
  ]
}
```

| Field | Meaning |
|-------|---------|
| `id` | Stable short id; use it in URLs |
| `format` | `pe` (Windows DLL), `mac-bundle`, or `native` (a library with a `.pipl` sidecar) |
| `archs` | Code the plug-in file contains: `win-x64`, `win-x86`, `win-arm64`, `mac-x64`, `mac-arm64`, `native` |
| `modes` | Image types the filter accepts: `rgb8`, `rgb16`, `gray8`, `gray16` |
| `runs` | Where a run would happen, or `null` when the filter cannot run here |
| `problem` | Why the filter cannot run here, or `null` |

`runs` reflects the server's isolation setting: with isolation on (the default), filters that could run in-process are reported as running in a worker process, because that is where the server runs them.

### `GET /v1/filters/{id}`

One filter: the same object as in the list under `filter`, plus the parsed PiPL resource under `pipl`. `{id}` may also be a name or `Category/Name` (URL-encoded); it is never treated as a file path.

### `POST /v1/filters/{id}/run`

Runs the filter on an image. There are two ways to send the request.

**Raw body.** The request body is the image file. Options go in the query string:

```console
$ curl -s --data-binary @photo.png -H "Content-Type: image/png" \
    "localhost:8780/v1/filters/8c8224326f39/run?format=jpeg" -o out.jpg
```

**Multipart.** A `multipart/form-data` body with these parts:

| Part | Required | Content |
|------|----------|---------|
| `image` | yes | The image file |
| `mask` | no | A selection mask the size of the image; its luminance is the selection, white is selected |
| `preset` | no | A preset file, as written by `8bf-host run --save-preset` |
| `options` | no | A JSON object with the options below |

Options in `options` override the same options in the query string. Any other part name is an error.

```console
$ curl -s -F image=@photo.png -F mask=@selection.png -F preset=@unsharp.json \
    -F 'options={"format": "png"};type=application/json' \
    localhost:8780/v1/filters/1a2b3c4d5e6f/run -o out.png
```

#### Options

| Option | Type | Default | Meaning |
|--------|------|---------|---------|
| `format` | string | the input's format | Output format: `png`, `jpeg`, `tiff`, `webp`, `bmp` or `pnm` |
| `rect` | string | whole image | `X,Y,WIDTH,HEIGHT`: only filter this rectangle |
| `foreground` | string | `#000000` | Foreground color handed to the filter |
| `background` | string | `#ffffff` | Background color handed to the filter |
| `resolution` | number | 72 | Resolution reported to the filter, in pixels per inch |
| `allow_8bit_fallback` | boolean | server setting | Run 8-bit-only filters on an 8-bit copy of 16-bit images |
| `response` | string | | `json` for a JSON response regardless of `Accept` |

Images are decoded as PNG, JPEG, TIFF, WebP, BMP or PNM, recognized by content rather than by `Content-Type`. The output keeps the input's channels and, where the format allows, its 16 bits per channel. JPEG output drops alpha.

#### Response

By default the response is the filtered image, with `Content-Type` set to the output format's MIME type.

With `Accept: application/json` (or `response=json`), the response is JSON instead:

```json
{
  "status": "ok",
  "content_type": "image/png",
  "image": "<base64>",
  "preset": {
    "format": "8bf-host-preset",
    "version": 1,
    "filter": {"name": "Invert", "category": "Wine", "id": "8c8224326f39"},
    "parameters": "NBIAAAEAAAA=",
    "descriptor": [{"key": "Runs", "type": "long", "value": 1}]
  },
  "log": []
}
```

`preset` holds the settings the filter ran with, in the preset file format, so a client can store it and send it back later. `log` holds the host's diagnostic lines; it is usually empty for successful runs.

Filters never show their dialogs on the server. A filter without a preset runs with its default settings.

### `POST /v1/rescan`

Scans the plug-in folders again, reusing cached results for files that have not changed. Use it after installing or removing filters.

```json
{"filters": 3, "problems": []}
```

`problems` lists plug-in files that could not be read, with the reason.

## Errors

Errors are JSON with an HTTP status that reflects the cause:

```json
{"error": {"kind": "filter_not_found", "message": "no filter named 'Blur'. Run '8bf-host list' to see the installed filters."}}
```

| Status | `kind` | Cause |
|--------|--------|-------|
| 400 | `bad_request` | Malformed options, form parts or preset; no image |
| 401 | `unauthorized` | Missing or wrong token |
| 404 | `filter_not_found` | No filter matches the id or name, or several do |
| 409 | `cancelled` | The filter cancelled the run itself |
| 413 | `bad_request` | The body is larger than `server.max_upload_mb` |
| 415 | `image_read` | The image could not be decoded, or exceeds `server.max_pixels` |
| 422 | `cannot_run` | The filter cannot run on this server (for example, no Wine worker) |
| 422 | `filter_failed` | The filter ran and reported an error; the message includes the host log |
| 500 | `internal` | A host error; the message says what failed |
| 504 | `timeout` | The run took longer than `server.timeout_secs` and was cancelled |

## Concurrency

At most `server.jobs` runs (default 2) execute at once; further requests wait for a slot. Each isolated run is a separate worker process. With `--no-isolate`, in-process runs are additionally serialized on one thread, since few filters are safe to run concurrently in one process; runs that need a worker of another architecture still use separate processes.
