"""Bundled app implementations."""

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from vibecast.apps.primevideo import PrimeVideo
    from vibecast.apps.svtplay import SvtPlay
    from vibecast.apps.viaplay import Viaplay

__all__ = ["PrimeVideo", "SvtPlay", "Viaplay"]


def __getattr__(name: str) -> Any:
    if name == "PrimeVideo":
        from vibecast.apps.primevideo import PrimeVideo

        return PrimeVideo
    if name == "SvtPlay":
        from vibecast.apps.svtplay import SvtPlay

        return SvtPlay
    if name == "Viaplay":
        from vibecast.apps.viaplay import Viaplay

        return Viaplay
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)
