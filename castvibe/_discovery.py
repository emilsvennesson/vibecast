"""mDNS advertisement for Cast device discovery."""

from __future__ import annotations

import hashlib
from typing import TYPE_CHECKING, Final, Protocol, cast

from pydantic import BaseModel, ConfigDict
from zeroconf import ServiceInfo
from zeroconf.asyncio import AsyncZeroconf

from castvibe._log import get_logger

if TYPE_CHECKING:
    from collections.abc import Awaitable

_GOOGLECAST_SERVICE_TYPE: Final[str] = "_googlecast._tcp.local."
_INSTANCE_PREFIX: Final[str] = "castvibe-"
_MAX_LABEL_LENGTH: Final[int] = 63

log = get_logger("discovery")


class _AsyncZeroconfLike(Protocol):
    """Typed subset of ``AsyncZeroconf`` used by this module.

    ``python-zeroconf`` currently exposes partial/unknown return types to
    static analyzers for these methods. This protocol keeps local type checking
    precise without changing runtime behavior.
    """

    async def async_register_service(
        self,
        info: ServiceInfo,
        ttl: int | None = None,
        allow_name_change: bool = False,
        cooperating_responders: bool = False,
        strict: bool = True,
    ) -> Awaitable[None]: ...

    async def async_unregister_service(self, info: ServiceInfo) -> Awaitable[None]: ...

    async def async_close(self) -> None: ...


class CastServiceTxt(BaseModel):
    """Structured TXT record payload for Cast mDNS advertisement."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    ve: str = "05"
    md: str
    fn: str
    id: str
    cd: str
    ca: str = "463365"
    bs: str
    st: str = "0"
    nf: str = "1"
    ic: str = "/setup/icon.png"
    rs: str = ""
    rm: str = ""


def _clean_device_id(device_id: str) -> str:
    return device_id.replace("-", "")


def _build_service_name(clean_id: str) -> str:
    max_id_len = _MAX_LABEL_LENGTH - len(_INSTANCE_PREFIX)
    truncated_id = clean_id[:max_id_len]
    instance = f"{_INSTANCE_PREFIX}{truncated_id}" if truncated_id else "castvibe"
    return f"{instance}.{_GOOGLECAST_SERVICE_TYPE}"


def _compute_bs(device_id: str) -> str:
    digest = hashlib.md5(device_id.encode("utf-8")).digest()  # noqa: S324
    return digest[:6].hex().upper()


class CastAdvertisement:
    """Advertises the receiver over mDNS as a Google Cast target."""

    __slots__ = (
        "_cert_digest",
        "_clean_id",
        "_device_id",
        "_device_model",
        "_friendly_name",
        "_info",
        "_port",
        "_service_name",
        "_txt",
        "_zeroconf",
    )

    def __init__(
        self,
        friendly_name: str,
        device_model: str,
        device_id: str,
        port: int,
        cert_digest: str,
    ) -> None:
        self._friendly_name = friendly_name
        self._device_model = device_model
        self._device_id = device_id
        self._clean_id = _clean_device_id(device_id)
        self._port = port
        self._cert_digest = cert_digest.upper()
        self._service_name = _build_service_name(self._clean_id)
        self._txt = CastServiceTxt(
            md=self._device_model,
            fn=self._friendly_name,
            id=self._clean_id,
            cd=self._cert_digest,
            bs=_compute_bs(self._device_id),
        )
        self._info: ServiceInfo | None = None
        self._zeroconf: _AsyncZeroconfLike | None = None

    @property
    def service_name(self) -> str:
        """Fully-qualified mDNS service instance name."""
        return self._service_name

    @property
    def service_type(self) -> str:
        """mDNS service type used for Cast receivers."""
        return _GOOGLECAST_SERVICE_TYPE

    @property
    def txt(self) -> CastServiceTxt:
        """Structured TXT record data for this service."""
        return self._txt

    @property
    def txt_records(self) -> dict[str, str]:
        """TXT records advertised for this cast service."""
        return cast("dict[str, str]", self._txt.model_dump())

    def _build_service_info(self) -> ServiceInfo:
        return ServiceInfo(
            type_=self.service_type,
            name=self.service_name,
            port=self._port,
            properties=self.txt_records,
        )

    async def start(self) -> None:
        """Start advertising this receiver on the local network."""
        if self._zeroconf is not None:
            return

        zeroconf = cast("_AsyncZeroconfLike", AsyncZeroconf())
        info = self._build_service_info()
        try:
            register_task = await zeroconf.async_register_service(info)
            await register_task
        except Exception:
            await zeroconf.async_close()
            raise

        self._info = info
        self._zeroconf = zeroconf
        log.info("registered mDNS cast service %s", self._service_name)

    async def stop(self) -> None:
        """Stop advertising and release mDNS resources."""
        zeroconf = self._zeroconf
        if zeroconf is None:
            return

        info = self._info
        self._info = None
        self._zeroconf = None

        try:
            if info is not None:
                unregister_task = await zeroconf.async_unregister_service(info)
                await unregister_task
        finally:
            await zeroconf.async_close()
            log.info("stopped mDNS cast service %s", self._service_name)


__all__ = ["CastAdvertisement", "CastServiceTxt"]
