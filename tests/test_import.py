def test_import() -> None:
    import castvibe  # noqa: F811

    assert castvibe.__doc__ is not None
