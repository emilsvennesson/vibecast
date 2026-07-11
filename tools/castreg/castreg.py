#!/usr/bin/env python3
"""castreg — query Google's Cast application registry.

Given a Cast **application id** (the 8-hex-digit `appId` a sender LAUNCHes, e.g.
`CA5E8412`), look up what it is and how a Cast device would run it: the display
name, the **web-receiver URL**, whether it launches via IPC, and its feature
whitelist. This is the same registry a real Chromecast queries at
`clients3.google.com/cast/chromecast/device/app?...&a=<appId>` before launching
an app.

Handy for identifying appIds captured by `tools/capture` (e.g. `cast.jsonl`
`GET_APP_AVAILABILITY` / `LAUNCH` messages) and for finding an app's receiver
URL. Pure standard library — run with `python3 castreg.py ...` or
`uv run castreg.py ...`.

Examples::

    python3 castreg.py app CA5E8412            # -> Netflix + receiver URL
    python3 castreg.py app 6313CF39 --raw      # full JSON
    python3 castreg.py scan                    # probe well-known/known appIds
    python3 castreg.py baseconfig              # device base config

Note: the registry returns an app's config regardless of the querying device;
whether a *specific* device will actually launch it is decided device-side (a
Shield, for instance, can report an app unavailable without querying here).
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.parse
import urllib.request

REGISTRY_URL = "https://clients3.google.com/cast/chromecast/device/app"
BASECONFIG_URL = "https://clients3.google.com/cast/chromecast/device/baseconfig"

# A generic Chromecast identity. The registry returns the same app config for
# any device, but the endpoint expects these device descriptors.
DEFAULT_DEVICE = {
    "m": "Chromecast",  # model
    "M": "Google Inc.",  # manufacturer
    "p": "anba",  # product/board
    "b": "1.56.500000",  # build
    "c": "ethernet",  # connection
    "r": "",
    "pt": "4",
    "at": "1",
}

# Confirmed / well-known appIds for `scan`. `scan` prints whatever the registry
# reports, so the comments are just hints — not authoritative.
KNOWN_APPS: dict[str, str] = {
    "CC1AD845": "Default Media Receiver",
    "233637DE": "YouTube",
    "CA5E8412": "Netflix",
    "6313CF39": "Viaplay",
    "95370A1C": "SVT Play",
}


def _device_params(args: argparse.Namespace) -> dict[str, str]:
    params = dict(DEFAULT_DEVICE)
    if args.model:
        params["m"] = args.model
    if args.make:
        params["M"] = args.make
    if args.product:
        params["p"] = args.product
    if args.build:
        params["b"] = args.build
    return params


def fetch_json(url: str, params: dict[str, str]) -> dict:
    """GET `url?params`, stripping Google's `)]}'` anti-hijack prefix."""
    query = urllib.parse.urlencode(params)
    request = urllib.request.Request(f"{url}?{query}", headers={"User-Agent": "castreg/0.1"})
    with urllib.request.urlopen(request, timeout=15) as response:  # noqa: S310 (trusted host)
        body = response.read().decode("utf-8", "replace")
    body = body.lstrip()
    if body.startswith(")]}'"):
        body = body[4:]
    return json.loads(body)


def resolve_app(appid: str, args: argparse.Namespace) -> dict:
    return fetch_json(REGISTRY_URL, {**_device_params(args), "a": appid.upper()})


def _print_app(data: dict) -> None:
    whitelisting = data.get("whitelisting", {})
    print(f"  appId         {data.get('app_id')}")
    print(f"  name          {data.get('display_name')}")
    print(f"  receiver_url  {data.get('url')}")
    print(f"  uses_ipc      {data.get('uses_ipc')}")
    print(f"  bg_mode       {data.get('background_mode_enabled')}")
    print(f"  features      {whitelisting.get('enabled_features')}")


def cmd_app(args: argparse.Namespace) -> int:
    try:
        data = resolve_app(args.appid, args)
    except urllib.error.HTTPError as error:
        print(f"registry returned HTTP {error.code} for {args.appid.upper()}", file=sys.stderr)
        return 1
    except (urllib.error.URLError, json.JSONDecodeError) as error:
        print(f"lookup failed: {error}", file=sys.stderr)
        return 1
    if args.raw:
        print(json.dumps(data, indent=2, ensure_ascii=False))
    else:
        _print_app(data)
    return 0


def cmd_scan(args: argparse.Namespace) -> int:
    appids = args.appid or list(KNOWN_APPS)
    if args.json:
        out = {}
        for appid in appids:
            try:
                out[appid.upper()] = resolve_app(appid, args)
            except Exception as error:  # noqa: BLE001 — report per-app, keep scanning
                out[appid.upper()] = {"_error": str(error)}
        print(json.dumps(out, indent=2, ensure_ascii=False))
        return 0
    print(f"{'appId':<10}  {'name':<28}  receiver_url")
    print(f"{'-' * 10}  {'-' * 28}  {'-' * 40}")
    for appid in appids:
        try:
            data = resolve_app(appid, args)
            name = str(data.get("display_name") or "?")
            url = str(data.get("url") or "")
        except Exception as error:  # noqa: BLE001
            name, url = f"<error: {error}>", ""
        print(f"{appid.upper():<10}  {name:<28}  {url}")
    return 0


def cmd_baseconfig(args: argparse.Namespace) -> int:
    try:
        data = fetch_json(BASECONFIG_URL, _device_params(args))
    except (urllib.error.URLError, json.JSONDecodeError) as error:
        print(f"lookup failed: {error}", file=sys.stderr)
        return 1
    if args.raw:
        print(json.dumps(data, indent=2, ensure_ascii=False))
    else:
        print("keys:", ", ".join(sorted(data.keys())))
    return 0


def _add_device_flags(p: argparse.ArgumentParser) -> None:
    p.add_argument("--model", help=f"device model 'm' (default: {DEFAULT_DEVICE['m']})")
    p.add_argument("--make", help=f"manufacturer 'M' (default: {DEFAULT_DEVICE['M']})")
    p.add_argument("--product", help=f"product/board 'p' (default: {DEFAULT_DEVICE['p']})")
    p.add_argument("--build", help=f"build 'b' (default: {DEFAULT_DEVICE['b']})")


def main() -> int:
    parser = argparse.ArgumentParser(prog="castreg", description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    p_app = sub.add_parser("app", help="resolve one appId to its receiver config")
    p_app.add_argument("appid", help="Cast application id, e.g. CA5E8412")
    p_app.add_argument("--raw", action="store_true", help="print the full JSON")
    _add_device_flags(p_app)
    p_app.set_defaults(func=cmd_app)

    p_scan = sub.add_parser("scan", help="resolve several appIds (default: known list)")
    p_scan.add_argument("appid", nargs="*", help="appIds to probe (default: built-in known list)")
    p_scan.add_argument("--json", action="store_true", help="print full JSON per appId")
    _add_device_flags(p_scan)
    p_scan.set_defaults(func=cmd_scan)

    p_base = sub.add_parser("baseconfig", help="fetch the device base config")
    p_base.add_argument("--raw", action="store_true", help="print the full JSON")
    _add_device_flags(p_base)
    p_base.set_defaults(func=cmd_baseconfig)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
