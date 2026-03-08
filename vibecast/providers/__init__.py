"""Bundled provider implementations."""

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from vibecast.providers.primevideo import PrimeVideoProvider
    from vibecast.providers.svtplay import SvtPlayProvider
    from vibecast.providers.viaplay import ViaplayProvider

__all__ = ["PrimeVideoProvider", "SvtPlayProvider", "ViaplayProvider"]


def __getattr__(name: str) -> Any:
    if name == "PrimeVideoProvider":
        from vibecast.providers.primevideo import PrimeVideoProvider

        return PrimeVideoProvider
    if name == "SvtPlayProvider":
        from vibecast.providers.svtplay import SvtPlayProvider

        return SvtPlayProvider
    if name == "ViaplayProvider":
        from vibecast.providers.viaplay import ViaplayProvider

        return ViaplayProvider
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)
