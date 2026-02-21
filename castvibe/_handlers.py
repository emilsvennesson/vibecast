"""Platform namespace handlers for the ``receiver-0`` transport."""

from __future__ import annotations

from typing import TYPE_CHECKING

from pydantic import ValidationError

import castvibe._namespace as ns
from castvibe._device import Device, build_receiver_status
from castvibe._log import get_logger
from castvibe._models import (
    AppAvailabilityResponse,
    ConnectRequest,
    DeviceInfoResponse,
    GetAppAvailabilityRequest,
    GetDeviceInfoRequest,
    GetStatusRequest,
    InvalidRequestResponse,
    LaunchErrorResponse,
    LaunchRequest,
    MultizoneGetStatusRequest,
    MultizoneStatus,
    MultizoneStatusResponse,
    SetupData,
    SetupDeviceInfo,
    SetupRequest,
    SetupResponse,
    SetVolumeRequest,
    StopRequest,
    connection_message_adapter,
    receiver_request_adapter,
)
from castvibe._util import extract_request_id, parse_json_payload
from castvibe.provider import LaunchCredentials, Provider

if TYPE_CHECKING:
    from collections.abc import Callable

    from castvibe._connection import Connection
    from castvibe._player_server import PlayerServer
    from castvibe._proto.cast_channel_pb2 import CastMessage
    from castvibe.player import Player

log = get_logger("handlers")


def _no_provider(_app_id: str) -> Provider | None:
    return None


class PlatformHandler:
    """Handles platform namespaces addressed to ``receiver-0``."""

    __slots__ = ("_device", "_player", "_player_server", "_provider_lookup")

    def __init__(
        self,
        device: Device,
        player: Player,
        player_server: PlayerServer | None,
        provider_lookup: Callable[[str], Provider | None] | None = None,
    ) -> None:
        self._device = device
        self._player = player
        self._player_server = player_server
        self._provider_lookup = provider_lookup or _no_provider

    async def handle_message(self, connection: Connection, msg: CastMessage) -> None:
        """Dispatch a transport message by namespace."""
        match msg.namespace:
            case ns.CONNECTION:
                await self._handle_connection(connection, msg)
            case ns.RECEIVER:
                await self._handle_receiver(connection, msg)
            case ns.DISCOVERY:
                await self._handle_discovery(connection, msg)
            case ns.MULTIZONE:
                await self._handle_multizone(connection, msg)
            case ns.SETUP:
                await self._handle_setup(connection, msg)
            case _:
                log.warning("unhandled platform namespace %s", msg.namespace)

    async def _handle_connection(
        self, connection: Connection, msg: CastMessage
    ) -> None:
        payload = parse_json_payload(msg)
        if payload is None:
            return

        try:
            request = connection_message_adapter.validate_python(payload)
        except ValidationError:
            log.warning("invalid connection payload", exc_info=True)
            return

        match request:
            case ConnectRequest():
                self._device.add_subscription(
                    connection, msg.source_id, msg.destination_id
                )
            case _:
                _ = self._device.remove_subscription(connection, msg.source_id)

    async def _handle_receiver(self, connection: Connection, msg: CastMessage) -> None:
        payload = parse_json_payload(msg)
        if payload is None:
            return

        try:
            request = receiver_request_adapter.validate_python(payload)
        except ValidationError:
            response = InvalidRequestResponse(
                request_id=extract_request_id(payload),
                reason="Invalid receiver request",
            )
            await self._device.send_to_sender(
                connection=connection,
                source_id=msg.destination_id,
                dest_id=msg.source_id,
                namespace=ns.RECEIVER,
                data=response.model_dump(exclude_none=True),
            )
            return

        match request:
            case GetStatusRequest():
                response = build_receiver_status(self._device, request.request_id)
                await self._device.send_to_sender(
                    connection=connection,
                    source_id=msg.destination_id,
                    dest_id=msg.source_id,
                    namespace=ns.RECEIVER,
                    data=response.model_dump(exclude_none=True),
                )
            case GetAppAvailabilityRequest():
                availability = dict.fromkeys(request.app_id, "APP_AVAILABLE")
                response = AppAvailabilityResponse(
                    request_id=request.request_id,
                    availability=availability,
                )
                await self._device.send_to_sender(
                    connection=connection,
                    source_id=msg.destination_id,
                    dest_id=msg.source_id,
                    namespace=ns.RECEIVER,
                    data=response.model_dump(exclude_none=True),
                )
            case LaunchRequest():
                await self._handle_launch_request(connection, msg, request)
            case StopRequest():
                _ = await self._device.stop_session(request.session_id)
                status = build_receiver_status(self._device, request.request_id)
                await self._device.broadcast(
                    source_id=msg.destination_id,
                    namespace=ns.RECEIVER,
                    data=status.model_dump(exclude_none=True),
                )
            case SetVolumeRequest():
                _update_volume(self._device, request)
                status = build_receiver_status(self._device, request.request_id)
                await self._device.broadcast(
                    source_id=msg.destination_id,
                    namespace=ns.RECEIVER,
                    data=status.model_dump(exclude_none=True),
                )

    async def _handle_launch_request(
        self,
        connection: Connection,
        msg: CastMessage,
        request: LaunchRequest,
    ) -> None:
        provider = self._provider_lookup(request.app_id)
        if provider is None:
            response = LaunchErrorResponse(
                request_id=request.request_id,
                reason="Application not available",
            )
            await self._device.send_to_sender(
                connection=connection,
                source_id=msg.destination_id,
                dest_id=msg.source_id,
                namespace=ns.RECEIVER,
                data=response.model_dump(exclude_none=True),
            )
            return

        credentials = _extract_launch_credentials(request)
        session = self._device.start_session(
            request.app_id,
            provider,
            credentials,
            player=self._player,
            player_server=self._player_server,
        )
        try:
            await session.on_launch(connection, msg.source_id)
        except Exception:
            _ = await self._device.stop_session(session.session_id)
            response = LaunchErrorResponse(
                request_id=request.request_id,
                reason="Application launch failed",
            )
            await self._device.send_to_sender(
                connection=connection,
                source_id=msg.destination_id,
                dest_id=msg.source_id,
                namespace=ns.RECEIVER,
                data=response.model_dump(exclude_none=True),
            )
            log.warning("provider launch callback failed", exc_info=True)
            return

        status = build_receiver_status(self._device, request.request_id)
        await self._device.broadcast(
            source_id=msg.destination_id,
            namespace=ns.RECEIVER,
            data=status.model_dump(exclude_none=True),
        )

    async def _handle_discovery(self, connection: Connection, msg: CastMessage) -> None:
        payload = parse_json_payload(msg)
        if payload is None:
            return

        try:
            request = GetDeviceInfoRequest.model_validate(payload)
        except ValidationError:
            log.warning("invalid discovery payload", exc_info=True)
            return

        response = DeviceInfoResponse(
            request_id=request.request_id,
            device_id=self._device.config.device_id,
            device_model=self._device.config.device_model,
            friendly_name=self._device.config.friendly_name,
        )
        await self._device.send_to_sender(
            connection=connection,
            source_id=msg.destination_id,
            dest_id=msg.source_id,
            namespace=ns.DISCOVERY,
            data=response.model_dump(exclude_none=True),
        )

    async def _handle_multizone(self, connection: Connection, msg: CastMessage) -> None:
        payload = parse_json_payload(msg)
        if payload is None:
            return

        try:
            request = MultizoneGetStatusRequest.model_validate(payload)
        except ValidationError:
            log.warning("invalid multizone payload", exc_info=True)
            return

        response = MultizoneStatusResponse(
            request_id=request.request_id,
            status=MultizoneStatus(
                devices=[],
                is_multichannel=False,
            ),
        )
        await self._device.send_to_sender(
            connection=connection,
            source_id=msg.destination_id,
            dest_id=msg.source_id,
            namespace=ns.MULTIZONE,
            data=response.model_dump(exclude_none=True),
        )

    async def _handle_setup(self, connection: Connection, msg: CastMessage) -> None:
        payload = parse_json_payload(msg)
        if payload is None:
            return

        try:
            request = SetupRequest.model_validate(payload)
        except ValidationError:
            log.warning("invalid setup payload", exc_info=True)
            return

        response = SetupResponse(
            request_id=request.request_id,
            response_code=200,
            response_string="OK",
            data=SetupData(
                name=self._device.config.friendly_name,
                version=8,
                device_info=SetupDeviceInfo(
                    ssdp_udn=self._device.config.ssdp_udn,
                ),
            ),
        )
        await self._device.send_to_sender(
            connection=connection,
            source_id=msg.destination_id,
            dest_id=msg.source_id,
            namespace=ns.SETUP,
            data=response.model_dump(exclude_none=True),
        )


def _update_volume(device: Device, request: SetVolumeRequest) -> None:
    """Apply SET_VOLUME updates without overwriting omitted fields."""
    fields_set = request.volume.model_fields_set
    if "level" in fields_set:
        device.volume.level = request.volume.level
    if "muted" in fields_set:
        device.volume.muted = request.volume.muted
    if "control_type" in fields_set:
        device.volume.control_type = request.volume.control_type
    if "step_interval" in fields_set:
        device.volume.step_interval = request.volume.step_interval


def _extract_launch_credentials(request: LaunchRequest) -> LaunchCredentials:
    nested = getattr(
        getattr(request.app_params, "launch_checker_params", None),
        "credentials_data",
        None,
    )

    credentials = _first_not_none(
        request.credentials,
        None if nested is None else nested.credentials,
    )
    credentials_type = _first_not_none(
        request.credentials_type,
        None if nested is None else nested.credentials_type,
    )

    return LaunchCredentials(
        credentials=credentials,
        credentials_type=credentials_type,
    )


def _first_not_none(primary: str | None, fallback: str | None) -> str | None:
    return primary if primary is not None else fallback


__all__ = ["PlatformHandler"]
