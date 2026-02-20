"""Tests for Cast mDNS advertisement."""

from __future__ import annotations

import asyncio
import hashlib
from typing import TYPE_CHECKING, Any
from unittest.mock import AsyncMock, patch
from uuid import uuid4

import pytest
from zeroconf import ServiceStateChange
from zeroconf.asyncio import AsyncServiceBrowser, AsyncServiceInfo, AsyncZeroconf

from castvibe._discovery import CastAdvertisement, CastServiceTxt

if TYPE_CHECKING:
    from castvibe._certificate import CertificateBundle


class TestTxtRecords:
    """TXT record and service-name construction tests."""

    def test_txt_records_match_expected_values(self, bundle: CertificateBundle) -> None:
        """Advertisement TXT records include expected Cast keys and values."""
        device_id = "3e3f3db0-1316-4f6f-a8db-d8d9aa123456"
        ad = CastAdvertisement(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id=device_id,
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        txt = ad.txt_records
        assert isinstance(ad.txt, CastServiceTxt)
        assert set(txt) == {
            "ve",
            "md",
            "fn",
            "id",
            "cd",
            "ca",
            "bs",
            "st",
            "nf",
            "ic",
            "rs",
            "rm",
        }

        assert txt["ve"] == "05"
        assert txt["md"] == "Chromecast"
        assert txt["fn"] == "Living Room"
        assert txt["id"] == "3e3f3db013164f6fa8dbd8d9aa123456"
        assert txt["cd"] == bundle.cert_digest_md5.upper()
        assert txt["ca"] == "463365"
        assert txt["st"] == "0"
        assert txt["nf"] == "1"
        assert txt["ic"] == "/setup/icon.png"
        assert txt["rs"] == ""
        assert txt["rm"] == ""

        expected_bs = hashlib.md5(device_id.encode("utf-8")).digest()[:6].hex().upper()  # noqa: S324
        assert txt["bs"] == expected_bs

    def test_service_name_uses_hyphenless_device_id(
        self, bundle: CertificateBundle
    ) -> None:
        """Service instance name includes device ID without hyphens."""
        device_id = "3e3f3db0-1316-4f6f-a8db-d8d9aa123456"
        ad = CastAdvertisement(
            friendly_name="Bedroom",
            device_model="Chromecast",
            device_id=device_id,
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        assert (
            ad.service_name
            == "castvibe-3e3f3db013164f6fa8dbd8d9aa123456._googlecast._tcp.local."
        )

    def test_service_name_truncates_to_mdns_label_limit(
        self, bundle: CertificateBundle
    ) -> None:
        """Service instance label never exceeds 63 characters."""
        device_id = "a" * 160
        ad = CastAdvertisement(
            friendly_name="Kitchen",
            device_model="Chromecast",
            device_id=device_id,
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        instance_label = ad.service_name.split(".", maxsplit=1)[0]
        assert len(instance_label) == 63
        assert instance_label.startswith("castvibe-")
        assert instance_label == f"castvibe-{device_id[:54]}"


class TestLifecycle:
    """Lifecycle tests for start/stop behavior."""

    async def test_start_and_stop_register_unregister_service(
        self, bundle: CertificateBundle
    ) -> None:
        """start() registers, stop() unregisters and closes zeroconf."""
        ad = CastAdvertisement(
            friendly_name="Office",
            device_model="Chromecast",
            device_id=str(uuid4()),
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        with patch("castvibe._discovery.AsyncZeroconf") as mock_ctor:
            mock_zeroconf = AsyncMock()
            mock_zeroconf.async_register_service.return_value = asyncio.create_task(
                asyncio.sleep(0)
            )
            mock_zeroconf.async_unregister_service.return_value = asyncio.create_task(
                asyncio.sleep(0)
            )
            mock_ctor.return_value = mock_zeroconf

            await ad.start()
            await ad.stop()

            mock_ctor.assert_called_once_with()
            mock_zeroconf.async_register_service.assert_awaited_once()
            registered_info = mock_zeroconf.async_register_service.await_args.args[0]
            mock_zeroconf.async_unregister_service.assert_awaited_once_with(
                registered_info
            )
            mock_zeroconf.async_close.assert_awaited_once()

    async def test_start_stop_are_idempotent(self, bundle: CertificateBundle) -> None:
        """Repeated start/stop calls do not duplicate zeroconf operations."""
        ad = CastAdvertisement(
            friendly_name="Office",
            device_model="Chromecast",
            device_id=str(uuid4()),
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        with patch("castvibe._discovery.AsyncZeroconf") as mock_ctor:
            mock_zeroconf = AsyncMock()
            mock_zeroconf.async_register_service.return_value = asyncio.create_task(
                asyncio.sleep(0)
            )
            mock_zeroconf.async_unregister_service.return_value = asyncio.create_task(
                asyncio.sleep(0)
            )
            mock_ctor.return_value = mock_zeroconf

            await ad.start()
            await ad.start()
            await ad.stop()
            await ad.stop()

            mock_ctor.assert_called_once_with()
            assert mock_zeroconf.async_register_service.await_count == 1
            assert mock_zeroconf.async_unregister_service.await_count == 1
            assert mock_zeroconf.async_close.await_count == 1

    async def test_start_failure_closes_zeroconf(
        self, bundle: CertificateBundle
    ) -> None:
        """If registration fails, zeroconf instance is still closed."""
        ad = CastAdvertisement(
            friendly_name="Office",
            device_model="Chromecast",
            device_id=str(uuid4()),
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        with patch("castvibe._discovery.AsyncZeroconf") as mock_ctor:
            mock_zeroconf = AsyncMock()
            mock_zeroconf.async_register_service.side_effect = RuntimeError("boom")
            mock_ctor.return_value = mock_zeroconf

            with pytest.raises(RuntimeError, match="boom"):
                await ad.start()

            mock_zeroconf.async_close.assert_awaited_once()


class TestIntegration:
    """Integration test using real zeroconf networking."""

    async def test_service_is_discoverable_and_removed(
        self, bundle: CertificateBundle
    ) -> None:
        """Advertisement appears in browser results and disappears after stop()."""
        ad = CastAdvertisement(
            friendly_name="CastVibe Test",
            device_model="Chromecast",
            device_id=str(uuid4()),
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )
        expected_txt = ad.txt_records

        added = asyncio.Event()
        removed = asyncio.Event()

        def on_service_state_change(**kwargs: Any) -> None:
            name = kwargs.get("name")
            state_change = kwargs.get("state_change")
            if not isinstance(name, str) or name != ad.service_name:
                return
            if state_change == ServiceStateChange.Added:
                _ = added.set()
            elif state_change == ServiceStateChange.Removed:
                _ = removed.set()

        observer = AsyncZeroconf()
        browser = AsyncServiceBrowser(
            observer.zeroconf,
            ad.service_type,
            handlers=[on_service_state_change],
        )

        try:
            await ad.start()

            try:
                _ = await asyncio.wait_for(added.wait(), timeout=8)
            except TimeoutError:
                pytest.skip("mDNS add event not observed in this environment")

            discovered = AsyncServiceInfo(ad.service_type, ad.service_name)
            _ = await discovered.async_request(observer.zeroconf, timeout=3000)
            props = discovered.decoded_properties

            assert props.get("ve") == expected_txt["ve"]
            assert props.get("md") == expected_txt["md"]
            assert props.get("fn") == expected_txt["fn"]
            assert props.get("id") == expected_txt["id"]
            assert props.get("cd") == expected_txt["cd"]
            assert props.get("ca") == expected_txt["ca"]
            assert props.get("bs") == expected_txt["bs"]
            assert props.get("st") == expected_txt["st"]
            assert props.get("nf") == expected_txt["nf"]
            assert props.get("ic") == expected_txt["ic"]
            assert props.get("rs") in (None, "")
            assert props.get("rm") in (None, "")

            await ad.stop()

            try:
                _ = await asyncio.wait_for(removed.wait(), timeout=8)
            except TimeoutError:
                pytest.skip("mDNS remove event not observed in this environment")
        finally:
            await ad.stop()
            await browser.async_cancel()
            await observer.async_close()
