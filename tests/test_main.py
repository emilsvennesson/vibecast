"""Tests for the CLI runtime wiring in ``vibecast.__main__``."""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Any

import pytest

import vibecast.__main__ as cli


class _FakeReceiver:
    def __init__(
        self,
        config: Any,
        certificates: Any,
        *,
        device_id: str,
        data_dir: Path,
    ) -> None:
        self.config = config
        self.certificates = certificates
        self.device_id = device_id
        self.data_dir = data_dir
        self.ran = False

    async def run_forever(self) -> None:
        self.ran = True


async def test_run_uses_certs_cli_override(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _ = (tmp_path / "config.toml").write_text(
        """
[device]
certs = "/from-config.json"
""".strip(),
        encoding="utf-8",
    )

    captured: dict[str, Any] = {}

    def fake_load_store(path: Path) -> object:
        captured["path"] = path
        return object()

    def fake_load_or_create_device_id(path: Path) -> str:
        captured["device_id_path"] = path
        return "device-123"

    def fake_receiver_ctor(
        config: Any,
        certificates: Any,
        *,
        device_id: str,
        data_dir: Path,
    ) -> _FakeReceiver:
        receiver = _FakeReceiver(
            config,
            certificates,
            device_id=device_id,
            data_dir=data_dir,
        )
        captured["receiver"] = receiver
        return receiver

    monkeypatch.setattr(
        cli,
        "_load_certificate_store",
        fake_load_store,
    )
    monkeypatch.setattr(cli, "_load_or_create_device_id", fake_load_or_create_device_id)
    monkeypatch.setattr(cli, "CastReceiver", fake_receiver_ctor)

    args = argparse.Namespace(
        certs=Path("/override-certs.json"),
        data_dir=tmp_path,
        log_level="INFO",
    )

    await cli._run(args)

    assert captured["path"] == Path("/override-certs.json")
    receiver = captured["receiver"]
    assert receiver.ran is True
    assert receiver.config.device.certs == "/override-certs.json"
    assert receiver.device_id == "device-123"
    assert receiver.data_dir == tmp_path


async def test_run_resolves_relative_certs_from_data_dir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _ = (tmp_path / "config.toml").write_text(
        """
[device]
certs = "certs.json"
""".strip(),
        encoding="utf-8",
    )

    captured: dict[str, Any] = {}

    def fake_load_store(path: Path) -> object:
        captured["path"] = path
        return object()

    def fake_load_or_create_device_id(path: Path) -> str:
        _ = path
        return "device-123"

    monkeypatch.setattr(cli, "_load_certificate_store", fake_load_store)
    monkeypatch.setattr(cli, "_load_or_create_device_id", fake_load_or_create_device_id)
    monkeypatch.setattr(cli, "CastReceiver", _FakeReceiver)

    args = argparse.Namespace(certs=None, data_dir=tmp_path, log_level="INFO")
    await cli._run(args)

    assert captured["path"] == tmp_path / "certs.json"


async def test_run_errors_when_certs_file_missing(tmp_path: Path) -> None:
    args = argparse.Namespace(
        certs=None,
        data_dir=tmp_path,
        log_level="INFO",
    )

    with pytest.raises(RuntimeError) as exc_info:
        await cli._run(args)

    message = str(exc_info.value)
    assert "certificate bundle file not found" in message
    assert str(tmp_path / "certs.json") in message
    assert (tmp_path / "config.toml").exists()
