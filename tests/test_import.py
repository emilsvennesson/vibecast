def test_import() -> None:
    import vibecast  # noqa: F811

    assert vibecast.__doc__ is not None
