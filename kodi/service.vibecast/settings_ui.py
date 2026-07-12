from __future__ import annotations

from typing import Any, Callable

import xbmcgui

from settings_model import AppSetting, AppSettings, SettingsCatalog


CONNECTION_LABEL_ID = 10
APP_NAME_LABEL_ID = 11
EMPTY_LABEL_ID = 12
APP_LIST_ID = 100
SETTING_LIST_ID = 200
RESET_BUTTON_ID = 201
CLOSE_BUTTON_ID = 300


class AppSettingsDialog(xbmcgui.WindowXMLDialog):
    def bind(
        self,
        catalog: SettingsCatalog,
        submit_change: Callable[[str, str, Any], None],
    ) -> None:
        self._catalog = catalog
        self._submit_change = submit_change
        self._initialized = False
        self._selected_app_key: str | None = None

    def onInit(self) -> None:  # noqa: N802
        self._initialized = True
        self.refresh()

    def onClick(self, control_id: int) -> None:  # noqa: N802
        if control_id == APP_LIST_ID:
            self._select_current_app()
            self._refresh_settings()
            return
        if control_id == SETTING_LIST_ID:
            self._edit_current_setting()
            return
        if control_id == RESET_BUTTON_ID:
            self._reset_current_setting()
            return
        if control_id == CLOSE_BUTTON_ID:
            self.close()

    def onAction(self, _action: xbmcgui.Action) -> None:  # noqa: N802
        if not self._initialized or self.getFocusId() != APP_LIST_ID:
            return
        previous_key = self._selected_app_key
        self._select_current_app()
        if self._selected_app_key != previous_key:
            self._refresh_settings()

    def refresh(self) -> None:
        if not self._initialized:
            return

        status = "Connected" if self._catalog.connected else "Disconnected - read only"
        self.getControl(CONNECTION_LABEL_ID).setLabel(status)

        apps = self._configurable_apps()
        app_list = self.getControl(APP_LIST_ID)
        previous_key = self._selected_app_key
        app_list.reset()
        for app in apps:
            suffix = " (updating)" if self._catalog.app_has_pending_update(app.app_key) else ""
            app_list.addItem(xbmcgui.ListItem(label=f"{app.display_name}{suffix}"))

        selected_index = next(
            (index for index, app in enumerate(apps) if app.app_key == previous_key),
            0,
        )
        if apps:
            app_list.selectItem(selected_index)
            self._selected_app_key = apps[selected_index].app_key
        else:
            self._selected_app_key = None
        self.getControl(EMPTY_LABEL_ID).setVisible(not apps)
        self._refresh_settings()

    def _select_current_app(self) -> None:
        position = self.getControl(APP_LIST_ID).getSelectedPosition()
        apps = self._configurable_apps()
        if 0 <= position < len(apps):
            self._selected_app_key = apps[position].app_key

    def _configurable_apps(self) -> tuple[AppSettings, ...]:
        return tuple(app for app in self._catalog.apps if app.settings)

    def _selected_app(self) -> AppSettings | None:
        if self._selected_app_key is None:
            return None
        return self._catalog.app(self._selected_app_key)

    def _selected_setting(self) -> AppSetting | None:
        app = self._selected_app()
        if app is None:
            return None
        position = self.getControl(SETTING_LIST_ID).getSelectedPosition()
        if position < 0 or position >= len(app.settings):
            return None
        return app.settings[position]

    def _refresh_settings(self) -> None:
        settings_list = self.getControl(SETTING_LIST_ID)
        selected_position = settings_list.getSelectedPosition()
        settings_list.reset()
        app = self._selected_app()
        if app is None:
            self.getControl(APP_NAME_LABEL_ID).setLabel("")
            return
        self.getControl(APP_NAME_LABEL_ID).setLabel(app.display_name)
        for setting in app.settings:
            item = xbmcgui.ListItem(
                label=setting.label,
                label2=_display_value(setting, setting.value),
            )
            item.setProperty("description", setting.description)
            settings_list.addItem(item)
        if app.settings:
            settings_list.selectItem(max(0, min(selected_position, len(app.settings) - 1)))

    def _edit_current_setting(self) -> None:
        app = self._selected_app()
        setting = self._selected_setting()
        if app is None or setting is None or not setting.writable or not self._catalog.connected:
            return

        selected, value = _prompt_for_value(setting)
        if selected:
            self._submit_change(app.app_key, setting.key, value)

    def _reset_current_setting(self) -> None:
        app = self._selected_app()
        setting = self._selected_setting()
        if app is None or setting is None or not setting.writable or not self._catalog.connected:
            return
        self._submit_change(app.app_key, setting.key, None)


def _prompt_for_value(setting: AppSetting) -> tuple[bool, Any]:
    if setting.options:
        labels = [option.label for option in setting.options]
        preselect = next(
            (
                index
                for index, option in enumerate(setting.options)
                if option.value == setting.value
            ),
            -1,
        )
        selected = xbmcgui.Dialog().select(setting.label, labels, preselect=preselect)
        if selected < 0:
            return False, None
        return True, setting.options[selected].value

    kind = setting.kind.lower()
    if kind in {"bool", "boolean", "toggle"}:
        selected = xbmcgui.Dialog().select(
            setting.label,
            ["Off", "On"],
            preselect=1 if setting.value is True else 0,
        )
        if selected < 0:
            return False, None
        return True, selected == 1

    current = setting.default if setting.value is None else setting.value
    if kind in {"int", "integer", "number", "float"}:
        entered = xbmcgui.Dialog().numeric(0, setting.label, str(current or ""))
        if entered == "":
            return False, None
        try:
            return True, float(entered) if kind in {"number", "float"} else int(entered)
        except ValueError:
            return False, None

    entered = xbmcgui.Dialog().input(setting.label, defaultt=str(current or ""))
    return True, entered


def _display_value(setting: AppSetting, value: Any) -> str:
    for option in setting.options:
        if option.value == value:
            return option.label
    if value is None:
        return f"Default: {_plain_value(setting.default)}"
    return _plain_value(value)


def _plain_value(value: Any) -> str:
    if value is True:
        return "On"
    if value is False:
        return "Off"
    if value is None:
        return "Not set"
    return str(value)
