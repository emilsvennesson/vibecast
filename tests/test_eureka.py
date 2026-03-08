"""Tests for eureka_info HTTP/HTTPS endpoints."""

from __future__ import annotations

from typing import TYPE_CHECKING

import httpx

from vibecast._config import EurekaDeviceCapabilitiesConfig
from vibecast._discovery.eureka import EurekaIdentity, EurekaServer

if TYPE_CHECKING:
    from vibecast._security.certificate import CertificateBundle


class TestEurekaServer:
    async def test_full_response_over_http(self, bundle: CertificateBundle) -> None:
        server = EurekaServer(
            bundle,
            EurekaIdentity(
                friendly_name="Living Room",
                device_model="Chromecast",
                ssdp_udn="11fc66ce-cbdb-4d9e-8845-8b45bea0d94d",
            ),
            host="127.0.0.1",
            https_port=0,
            http_port=0,
        )

        await server.start(certificates=bundle)
        try:
            assert server.http_serving_port is not None

            async with httpx.AsyncClient(timeout=5.0) as client:
                response = await client.get(
                    f"http://127.0.0.1:{server.http_serving_port}/setup/eureka_info"
                )
            _ = response.raise_for_status()
            payload = response.json()

            assert payload["name"] == "Living Room"
            assert payload["ssdp_udn"] == "11fc66ce-cbdb-4d9e-8845-8b45bea0d94d"
            assert payload["version"] == 12
            assert payload["connected"] is True
            assert "device_info" not in payload
            assert "multizone" not in payload
        finally:
            await server.stop()

    async def test_filtered_response_over_https(
        self, bundle: CertificateBundle
    ) -> None:
        server = EurekaServer(
            bundle,
            EurekaIdentity(
                friendly_name="Kitchen",
                device_model="Chromecast",
                ssdp_udn="9f6cab7f-91f0-45fc-88ea-6e113f0f8b1a",
            ),
            host="127.0.0.1",
            https_port=0,
            http_port=0,
        )

        await server.start(certificates=bundle)
        try:
            assert server.https_serving_port is not None

            async with httpx.AsyncClient(verify=False, timeout=5.0) as client:
                response = await client.get(
                    f"https://127.0.0.1:{server.https_serving_port}/setup/eureka_info",
                    params={"params": "device_info,name,multizone"},
                )
            _ = response.raise_for_status()
            payload = response.json()

            assert set(payload) == {"device_info", "name", "multizone"}
            assert payload["name"] == "Kitchen"
            assert (
                payload["device_info"]["ssdp_udn"]
                == "9f6cab7f-91f0-45fc-88ea-6e113f0f8b1a"
            )
            assert payload["device_info"]["capabilities"]["display_supported"] is True
            assert payload["multizone"]["max_static_groups"] == 100
        finally:
            await server.stop()

    async def test_identity_fields_override_defaults(
        self,
        bundle: CertificateBundle,
    ) -> None:
        server = EurekaServer(
            bundle,
            EurekaIdentity(
                friendly_name="Bedroom",
                device_model="Chromecast Ultra",
                ssdp_udn="f0f2dd5f-c123-4b85-8529-e821291ec31a",
                manufacturer="Acme Devices",
                locale="sv-SE",
                country_code="SE",
                build_version="999999",
                build_revision="9.9.999999",
                capabilities=EurekaDeviceCapabilitiesConfig(display_supported=False),
            ),
            host="127.0.0.1",
            https_port=0,
            http_port=0,
        )

        await server.start(certificates=bundle)
        try:
            assert server.http_serving_port is not None

            async with httpx.AsyncClient(timeout=5.0) as client:
                response = await client.get(
                    f"http://127.0.0.1:{server.http_serving_port}/setup/eureka_info",
                    params={
                        "params": "build_version,cast_build_revision,locale,location,device_info"
                    },
                )
            _ = response.raise_for_status()
            payload = response.json()

            assert payload["build_version"] == "999999"
            assert payload["cast_build_revision"] == "9.9.999999"
            assert payload["locale"] == "sv-SE"
            assert payload["location"]["country_code"] == "SE"
            assert payload["device_info"]["manufacturer"] == "Acme Devices"
            assert payload["device_info"]["capabilities"]["display_supported"] is False
        finally:
            await server.stop()
