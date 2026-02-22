"""Setup namespace messages (eureka_info)."""

from typing import Literal

from pydantic import AliasChoices, Field, TypeAdapter

from vibecast._models._base import CastModel


class SetupRequest(CastModel):
    """Inbound setup request from sender."""

    type: Literal["eureka_info"] = "eureka_info"
    request_id: int = Field(
        validation_alias=AliasChoices("request_id", "requestId"),
        serialization_alias="request_id",
    )


class SetupDeviceInfo(CastModel):
    """Subset of setup device information fields."""

    ssdp_udn: str = Field(serialization_alias="ssdp_udn")


class SetupData(CastModel):
    """Setup response data block."""

    device_info: SetupDeviceInfo = Field(serialization_alias="device_info")
    name: str
    version: int


class SetupResponse(CastModel):
    """Outbound setup response returned for eureka_info."""

    type: Literal["eureka_info"] = "eureka_info"
    request_id: int = Field(
        validation_alias=AliasChoices("request_id", "requestId"),
        serialization_alias="request_id",
    )
    response_code: int = Field(serialization_alias="response_code")
    response_string: str = Field(serialization_alias="response_string")
    data: SetupData


setup_request_adapter: TypeAdapter[SetupRequest] = TypeAdapter(SetupRequest)
