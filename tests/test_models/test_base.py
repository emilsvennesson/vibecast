"""Tests for the CastModel base class."""

from castvibe._models._base import CastModel


class _SampleModel(CastModel):
    """Minimal model for testing CastModel behavior."""

    foo_bar: str = ""
    count: int = 0


class TestCamelCaseSerialization:
    """CastModel serializes snake_case fields as camelCase."""

    def test_dump_produces_camel_case(self) -> None:
        m = _SampleModel(foo_bar="hello", count=42)
        data = m.model_dump(exclude_none=True)
        assert "fooBar" in data
        assert "foo_bar" not in data
        assert data["fooBar"] == "hello"
        assert data["count"] == 42

    def test_dump_json_produces_camel_case(self) -> None:
        m = _SampleModel(foo_bar="x")
        raw = m.model_dump_json()
        assert '"fooBar"' in raw


class TestPopulateByName:
    """CastModel accepts both snake_case and camelCase construction."""

    def test_construct_with_snake_case(self) -> None:
        m = _SampleModel(foo_bar="snake")
        assert m.foo_bar == "snake"

    def test_construct_with_camel_case(self) -> None:
        m = _SampleModel.model_validate({"fooBar": "camel", "count": 7})
        assert m.foo_bar == "camel"
        assert m.count == 7


class TestExtraFieldsPreserved:
    """CastModel preserves unknown fields (extra='allow')."""

    def test_unknown_fields_kept(self) -> None:
        m = _SampleModel.model_validate(
            {"fooBar": "hi", "unknownField": 99, "another": "data"}
        )
        assert m.foo_bar == "hi"
        data = m.model_dump()
        assert data["unknownField"] == 99
        assert data["another"] == "data"

    def test_extra_fields_survive_round_trip(self) -> None:
        raw = {"fooBar": "ok", "count": 1, "secretSauce": True}
        m = _SampleModel.model_validate(raw)
        dumped = m.model_dump()
        assert dumped["secretSauce"] is True
