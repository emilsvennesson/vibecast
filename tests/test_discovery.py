"""Tests for Cast mDNS advertisement."""

from __future__ import annotations

import asyncio
import hashlib
import socket
from typing import TYPE_CHECKING, Any
from unittest.mock import AsyncMock, MagicMock, patch
from uuid import uuid4

import pytest
from zeroconf import ServiceStateChange
from zeroconf.asyncio import AsyncServiceBrowser, AsyncServiceInfo, AsyncZeroconf

from vibecast._discovery import CastAdvertisement, CastServiceTxt

if TYPE_CHECKING:
    from vibecast._certificate import CertificateBundle


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
            == "vibecast-3e3f3db013164f6fa8dbd8d9aa123456._googlecast._tcp.local."
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
        assert instance_label.startswith("vibecast-")
        assert instance_label == f"vibecast-{device_id[:54]}"

    def test_server_name_uses_hyphenated_uuid_local(
        self, bundle: CertificateBundle
    ) -> None:
        """SRV host target follows the Shield-style ``<uuid>.local.`` format."""
        device_id = "3e3f3db0-1316-4f6f-a8db-d8d9aa123456"
        ad = CastAdvertisement(
            friendly_name="Bedroom",
            device_model="Chromecast",
            device_id=device_id,
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        assert ad.server == "3e3f3db0-1316-4f6f-a8db-d8d9aa123456.local."

    def test_server_name_canonicalizes_hyphenless_uuid(
        self, bundle: CertificateBundle
    ) -> None:
        """Hyphenless UUID-like IDs are normalized for SRV host target."""
        ad = CastAdvertisement(
            friendly_name="Bedroom",
            device_model="Chromecast",
            device_id="3e3f3db013164f6fa8dbd8d9aa123456",
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        assert ad.server == "3e3f3db0-1316-4f6f-a8db-d8d9aa123456.local."

    def test_parsed_addresses_contains_at_least_one_ipv4(
        self, bundle: CertificateBundle
    ) -> None:
        """Advertisement always provides at least one IPv4 address."""
        ad = CastAdvertisement(
            friendly_name="Bedroom",
            device_model="Chromecast",
            device_id=str(uuid4()),
            port=8009,
            cert_digest=bundle.cert_digest_md5,
        )

        assert ad.parsed_addresses

    def test_parsed_addresses_filter_loopback_getaddrinfo(
        self, bundle: CertificateBundle
    ) -> None:
        """Loopback results from getaddrinfo are filtered out."""
        getaddrinfo_result = [
            (socket.AF_INET, socket.SOCK_DGRAM, 0, "", ("127.0.0.1", 0)),
            (socket.AF_INET, socket.SOCK_DGRAM, 0, "", ("192.168.10.20", 0)),
        ]

        with (
            patch(
                "vibecast._discovery.socket.getaddrinfo",
                return_value=getaddrinfo_result,
            ),
            patch("vibecast._discovery.socket.socket", side_effect=OSError),
        ):
            ad = CastAdvertisement(
                friendly_name="Bedroom",
                device_model="Chromecast",
                device_id=str(uuid4()),
                port=8009,
                cert_digest=bundle.cert_digest_md5,
            )

        assert ad.parsed_addresses == ("192.168.10.20",)

    def test_parsed_addresses_use_udp_probe_when_getaddrinfo_empty(
        self, bundle: CertificateBundle
    ) -> None:
        """UDP probe address is used when hostname lookup has no addresses."""
        fake_socket = MagicMock()
        fake_socket.__enter__.return_value = fake_socket
        fake_socket.getsockname.return_value = ("10.0.0.55", 42424)

        with (
            patch("vibecast._discovery.socket.getaddrinfo", return_value=[]),
            patch("vibecast._discovery.socket.socket", return_value=fake_socket),
        ):
            ad = CastAdvertisement(
                friendly_name="Bedroom",
                device_model="Chromecast",
                device_id=str(uuid4()),
                port=8009,
                cert_digest=bundle.cert_digest_md5,
            )

        assert ad.parsed_addresses == ("10.0.0.55",)
        fake_socket.connect.assert_called_once_with(("224.0.0.251", 5353))

    def test_parsed_addresses_fallback_to_loopback_on_failures(
        self, bundle: CertificateBundle
    ) -> None:
        """Loopback fallback is used when all address lookups fail."""
        with (
            patch("vibecast._discovery.socket.getaddrinfo", side_effect=OSError),
            patch("vibecast._discovery.socket.socket", side_effect=OSError),
        ):
            ad = CastAdvertisement(
                friendly_name="Bedroom",
                device_model="Chromecast",
                device_id=str(uuid4()),
                port=8009,
                cert_digest=bundle.cert_digest_md5,
            )

        assert ad.parsed_addresses == ("127.0.0.1",)

    def test_app_subtype_types_are_advertised(self, bundle: CertificateBundle) -> None:
        """Valid app IDs generate Cast subtype DNS-SD records."""
        ad = CastAdvertisement(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="3e3f3db0-1316-4f6f-a8db-d8d9aa123456",
            port=8009,
            cert_digest=bundle.cert_digest_md5,
            app_ids={"6313CF39", "cc1ad845", "invalid", "TOO-LONG-APP-ID"},
        )

        subtype_infos = ad._build_all_service_infos()[1:]
        subtype_types = {info.type for info in subtype_infos}

        assert subtype_types == {
            "_6313CF39._sub._googlecast._tcp.local.",
            "_CC1AD845._sub._googlecast._tcp.local.",
        }


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

        with patch("vibecast._discovery.AsyncZeroconf") as mock_ctor:
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
            assert registered_info.server == ad.server
            assert registered_info.parsed_addresses()
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

        with patch("vibecast._discovery.AsyncZeroconf") as mock_ctor:
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

    async def test_start_registers_subtypes_when_app_ids_present(
        self, bundle: CertificateBundle
    ) -> None:
        """Subtype registrations are added for each valid app ID."""
        ad = CastAdvertisement(
            friendly_name="Office",
            device_model="Chromecast",
            device_id=str(uuid4()),
            port=8009,
            cert_digest=bundle.cert_digest_md5,
            app_ids={"6313CF39", "0F5096E8"},
        )

        with patch("vibecast._discovery.AsyncZeroconf") as mock_ctor:
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

            assert mock_zeroconf.async_register_service.await_count == 3
            assert mock_zeroconf.async_unregister_service.await_count == 3

    async def test_stop_continues_cleanup_when_unregister_or_close_fail(
        self, bundle: CertificateBundle
    ) -> None:
        """stop() attempts cleanup for all registrations even after failures."""
        ad = CastAdvertisement(
            friendly_name="Office",
            device_model="Chromecast",
            device_id=str(uuid4()),
            port=8009,
            cert_digest=bundle.cert_digest_md5,
            app_ids={"6313CF39", "0F5096E8"},
        )

        with patch("vibecast._discovery.AsyncZeroconf") as mock_ctor:
            base_zeroconf = AsyncMock()
            subtype_zeroconf_a = AsyncMock()
            subtype_zeroconf_b = AsyncMock()
            zeroconfs = [base_zeroconf, subtype_zeroconf_a, subtype_zeroconf_b]

            for mock_zeroconf in zeroconfs:
                mock_zeroconf.async_register_service.return_value = asyncio.create_task(
                    asyncio.sleep(0)
                )
                mock_zeroconf.async_unregister_service.return_value = (
                    asyncio.create_task(asyncio.sleep(0))
                )

            subtype_zeroconf_b.async_unregister_service.side_effect = RuntimeError(
                "unregister failed"
            )
            subtype_zeroconf_a.async_close.side_effect = RuntimeError("close failed")

            mock_ctor.side_effect = zeroconfs

            await ad.start()
            await ad.stop()

            assert mock_ctor.call_count == 3
            for mock_zeroconf in zeroconfs:
                mock_zeroconf.async_register_service.assert_awaited_once()
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

        with patch("vibecast._discovery.AsyncZeroconf") as mock_ctor:
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
            friendly_name="vibecast Test",
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
            resolved = await discovered.async_request(observer.zeroconf, timeout=3000)
            if not resolved:
                pytest.skip("mDNS service details not resolvable in this environment")
            props = discovered.decoded_properties
            addresses = discovered.parsed_addresses()

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
            assert discovered.server == ad.server
            assert set(addresses).intersection(ad.parsed_addresses)

            await ad.stop()

            try:
                _ = await asyncio.wait_for(removed.wait(), timeout=8)
            except TimeoutError:
                pytest.skip("mDNS remove event not observed in this environment")
        finally:
            await ad.stop()
            await browser.async_cancel()
            await observer.async_close()
