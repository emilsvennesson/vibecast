# castreg (dev tool)

A tiny CLI for querying **Google's Cast application registry** — the same
endpoint (`clients3.google.com/cast/chromecast/device/app`) a real Chromecast
hits to resolve an app before launching it. Given a Cast **application id** (the
8-hex `appId` a sender `LAUNCH`es, e.g. `CA5E8412`), it tells you the app's
name, its **web-receiver URL**, whether it launches via IPC, and its feature
whitelist.

Useful for identifying appIds seen by `tools/capture` (in `cast.jsonl`
`GET_APP_AVAILABILITY` / `LAUNCH` messages) and for finding an app's receiver
URL when reverse-engineering it for a vibecast app provider.

Standard library only — no install needed.

## Usage

```sh
# Resolve one appId
python3 castreg.py app CA5E8412
#   appId         CA5E8412
#   name          Netflix
#   receiver_url  https://cast-uiboot.prod.cloud.netflix.com/nq/mdx/eureka/^1.0.0/bootloader
#   uses_ipc      True
#   features      67071

python3 castreg.py app 6313CF39 --raw     # full JSON

# Probe a set of appIds (defaults to a small known list; identifies each)
python3 castreg.py scan
python3 castreg.py scan CA5E8412 233637DE 9AC194DC     # your own ids
python3 castreg.py scan --json > apps.json

# Device base config
python3 castreg.py baseconfig --raw
```

`uv run castreg.py ...` works too (there are no dependencies).

Optional device descriptors (`--model`, `--make`, `--product`, `--build`) set
the querying device identity; the registry returns the same app config
regardless, so they rarely matter.

## Good to know

- The registry resolving an app does **not** mean a given device will launch it.
  Availability is decided device-side: a Shield, for example, can report Netflix
  `APP_UNAVAILABLE` (and `LAUNCH_ERROR: NOT_FOUND`) without ever querying the
  registry, even though Netflix's web receiver is still registered here.
- `receiver_url` is the web receiver a Chromecast-class device loads to run the
  app — the starting point if you ever want vibecast to host that receiver.
- Some known appIds: `CC1AD845` Default Media Receiver, `233637DE` YouTube,
  `CA5E8412` Netflix, `6313CF39` Viaplay, `95370A1C` SVT Play.
