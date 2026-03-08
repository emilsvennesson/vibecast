"""Receiver status builders."""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

from vibecast._models import (
    ApplicationStatus,
    CastNamespace,
    ReceiverStatus,
    ReceiverStatusResponse,
    Volume,
)


class _TransportLike(Protocol):
    @property
    def subscriptions(self) -> Sequence[object]: ...


class _SessionLike(Protocol):
    app_id: str
    display_name: str
    session_id: str
    transport_id: str
    icon_url: str | None
    status_text: str
    namespaces: tuple[str, ...]


class DeviceStateView(Protocol):
    @property
    def transports(self) -> Mapping[str, _TransportLike]: ...

    def snapshot_receiver_state(self) -> tuple[Sequence[_SessionLike], Volume]: ...


def build_receiver_status(
    device: DeviceStateView,
    request_id: int = 0,
) -> ReceiverStatusResponse:
    """Build a ``RECEIVER_STATUS`` response from current device state."""
    sessions, volume = device.snapshot_receiver_state()

    applications: list[ApplicationStatus] = []
    for session in sessions:
        transport = device.transports.get(session.transport_id)
        sender_connected = bool(transport and transport.subscriptions)
        applications.append(
            ApplicationStatus(
                app_id=session.app_id,
                app_type="WEB",
                display_name=session.display_name,
                icon_url=session.icon_url,
                is_idle_screen=False,
                launched_from_cloud=False,
                namespaces=[CastNamespace(name=name) for name in session.namespaces],
                sender_connected=sender_connected,
                session_id=session.session_id,
                status_text=session.status_text,
                transport_id=session.transport_id,
                universal_app_id=session.app_id,
            )
        )

    status = ReceiverStatus(
        applications=applications,
        volume=volume,
        is_active_input=True,
        is_stand_by=False,
    )
    return ReceiverStatusResponse(request_id=request_id, status=status)


__all__ = ["DeviceStateView", "build_receiver_status"]
