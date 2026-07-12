from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from protocol import ProtocolError, settings_update_message


@dataclass(slots=True, frozen=True)
class SettingOption:
    value: Any
    label: str


@dataclass(slots=True, frozen=True)
class AppSetting:
    key: str
    label: str
    description: str
    kind: str
    default: Any
    value: Any
    writable: bool
    options: tuple[SettingOption, ...]

    @classmethod
    def from_wire(cls, raw: Any) -> AppSetting:
        if not isinstance(raw, dict):
            raise ProtocolError("setting must be an object")

        key = _required_string(raw, "key")
        label = _required_string(raw, "label")
        kind = _required_string(raw, "kind")
        description = raw.get("description", "")
        if not isinstance(description, str):
            raise ProtocolError(f"setting {key!r} description must be a string")

        raw_options = raw.get("options", [])
        if not isinstance(raw_options, list):
            raise ProtocolError(f"setting {key!r} options must be a list")
        options = tuple(_option_from_wire(option, key) for option in raw_options)
        writable = raw.get("writable")
        if not isinstance(writable, bool):
            raise ProtocolError(f"setting {key!r} writable must be a boolean")

        return cls(
            key=key,
            label=label,
            description=description,
            kind=kind,
            default=raw.get("default"),
            value=raw.get("value"),
            writable=writable,
            options=options,
        )


@dataclass(slots=True, frozen=True)
class AppSettings:
    app_key: str
    display_name: str
    revision: int
    settings: tuple[AppSetting, ...]

    @classmethod
    def from_wire(cls, raw: Any) -> AppSettings:
        if not isinstance(raw, dict):
            raise ProtocolError("app settings must be an object")

        app_key = _required_string(raw, "appKey")
        display_name = _required_string(raw, "displayName")
        revision = raw.get("revision")
        if isinstance(revision, bool) or not isinstance(revision, int) or revision < 0:
            raise ProtocolError(f"app {app_key!r} revision must be a non-negative integer")

        raw_settings = raw.get("settings")
        if not isinstance(raw_settings, list):
            raise ProtocolError(f"app {app_key!r} settings must be a list")
        settings = tuple(AppSetting.from_wire(setting) for setting in raw_settings)
        if len({setting.key for setting in settings}) != len(settings):
            raise ProtocolError(f"app {app_key!r} contains duplicate setting keys")

        return cls(
            app_key=app_key,
            display_name=display_name,
            revision=revision,
            settings=settings,
        )

    def setting(self, key: str) -> AppSetting | None:
        return next((setting for setting in self.settings if setting.key == key), None)


class SettingsCatalog:
    def __init__(self) -> None:
        self._apps: tuple[AppSettings, ...] = ()
        self._pending: dict[str, str] = {}
        self._connected = False

    @property
    def apps(self) -> tuple[AppSettings, ...]:
        return self._apps

    @property
    def connected(self) -> bool:
        return self._connected

    def set_connected(self, connected: bool) -> None:
        self._connected = connected
        if not connected:
            self._pending.clear()

    def replace_snapshot(self, payload: dict[str, Any]) -> None:
        raw_apps = payload.get("apps")
        if not isinstance(raw_apps, list):
            raise ProtocolError("settings snapshot apps must be a list")
        apps = tuple(AppSettings.from_wire(app) for app in raw_apps)
        if len({app.app_key for app in apps}) != len(apps):
            raise ProtocolError("settings snapshot contains duplicate app keys")
        self._apps = apps
        self._pending.clear()

    def apply_update_result(self, payload: dict[str, Any]) -> str:
        request_id = _required_string(payload, "requestId")
        status = _required_string(payload, "status")
        app = AppSettings.from_wire(payload.get("app"))
        pending_app_key = self._pending.get(request_id)
        if pending_app_key is None:
            raise ProtocolError(f"unknown settings request {request_id!r}")
        if pending_app_key != app.app_key:
            raise ProtocolError("settings result app does not match its request")
        self._replace_app(app)
        self._pending.pop(request_id)
        return status

    def begin_update(
        self,
        request_id: str,
        app_key: str,
        setting_key: str,
        value: Any,
    ) -> dict[str, Any]:
        if not self._connected:
            raise ProtocolError("settings are read-only while disconnected")
        app = self.app(app_key)
        if app is None:
            raise ProtocolError(f"unknown app {app_key!r}")
        if self.app_has_pending_update(app_key):
            raise ProtocolError(f"app {app_key!r} already has an update pending")
        setting = app.setting(setting_key)
        if setting is None:
            raise ProtocolError(f"unknown setting {setting_key!r}")
        if not setting.writable:
            raise ProtocolError(f"setting {setting_key!r} is read-only")

        message = settings_update_message(
            request_id,
            app_key,
            app.revision,
            {setting_key: value},
        )
        self._pending[request_id] = app_key
        return message

    def app(self, app_key: str) -> AppSettings | None:
        return next((app for app in self._apps if app.app_key == app_key), None)

    def app_has_pending_update(self, app_key: str) -> bool:
        return app_key in self._pending.values()

    def _replace_app(self, replacement: AppSettings) -> None:
        apps = list(self._apps)
        for index, app in enumerate(apps):
            if app.app_key == replacement.app_key:
                apps[index] = replacement
                self._apps = tuple(apps)
                return
        apps.append(replacement)
        self._apps = tuple(apps)


def _required_string(raw: dict[str, Any], key: str) -> str:
    value = raw.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ProtocolError(f"{key} must be a non-empty string")
    return value.strip()


def _option_from_wire(raw: Any, setting_key: str) -> SettingOption:
    if not isinstance(raw, dict):
        raise ProtocolError(f"setting {setting_key!r} option must be an object")
    label = _required_string(raw, "label")
    if "value" not in raw:
        raise ProtocolError(f"setting {setting_key!r} option must have a value")
    return SettingOption(value=raw["value"], label=label)
