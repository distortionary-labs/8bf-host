# Using the HTTP server

`8bf-host serve` offers the same filters over HTTP:

```console
$ 8bf-host serve
8bf-host 0.1.0 serving 3 filters on http://127.0.0.1:8780 (isolated workers)
```

The API is small. The full reference, with every option and error, is in [http-api.md](http-api.md).

```console
$ curl -s localhost:8780/v1/health
{"status":"ok","version":"0.1.0"}
$ curl -s localhost:8780/v1/filters
{"filters":[{"archs":["win-x64"],"category":"Wine","format":"pe","has_about_box":true,"id":"443b91e33c0e", ...snip...
$ curl -s --data-binary @photo.png -H "Content-Type: image/png" \
    localhost:8780/v1/filters/8c8224326f39/run -o inverted.png
```

The last request sends the image as the request body and receives the filtered image back. Options go in the query string (`?format=jpeg&rect=0,0,100,100`). To send a mask or a preset along with the image, use a `multipart/form-data` request instead:

```console
$ curl -s -F image=@photo.png -F mask=@selection.png -F preset=@unsharp.json \
    localhost:8780/v1/filters/1a2b3c4d5e6f/run -o out.png
```

Errors come back as JSON with an HTTP status that reflects the cause, for example `404` for an unknown filter and `422` when the filter itself fails:

```console
$ curl -s --data-binary @photo.png localhost:8780/v1/filters/443b91e33c0e/run
{"error":{"kind":"filter_failed","message":"Could not complete the filter because the test filter failed on purpose"}}
```

## Running the server safely

A filter is native code, and the server runs it on request. A few defaults follow from that:

- **Isolation.** Every run happens in a separate worker process, so a filter that crashes fails one request instead of stopping the server. `--no-isolate` runs filters inside the server process instead. That saves the cost of starting a process per run, and it is reasonable for filters you have tested. In-process runs go through a single thread, one at a time, because few filters are written to run concurrently.
- **Loopback by default.** The server listens on `127.0.0.1:8780`. It refuses to listen on any other address unless a token is set with `EIGHTBF_HOST_TOKEN` (or `server.token`); clients then send `Authorization: Bearer <token>`. If the network is trusted, `--insecure-no-auth` overrides the check. The token is not a substitute for TLS: put a reverse proxy such as Caddy in front of the server if it is reachable beyond one machine.
- **No uploads of code.** Clients pick filters by id or name from the scanned folders. A request cannot name a file for the server to load.
- **Limits.** Request bodies are limited to 256 MB, images to 100 megapixels, and runs to 300 seconds; a run that takes longer is cancelled and the client receives `504`. All three are configurable.
