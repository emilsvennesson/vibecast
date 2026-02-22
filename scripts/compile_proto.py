"""Compile cast_channel.proto into Python protobuf modules.

Generates cast_channel_pb2.py and cast_channel_pb2.pyi in vibecast/_proto/
so that library consumers don't need protoc installed.

Usage:
    uv run python scripts/compile_proto.py
"""

from pathlib import Path

from grpc_tools import protoc

REPO_ROOT = Path(__file__).resolve().parent.parent
PROTO_DIR = REPO_ROOT / "vibecast" / "_proto"
PROTO_FILE = PROTO_DIR / "cast_channel.proto"


def main() -> None:
    if not PROTO_FILE.exists():
        msg = f"Proto file not found: {PROTO_FILE}"
        raise FileNotFoundError(msg)

    # grpc_tools.protoc is untyped; suppress unknown-type warnings.
    result: int = protoc.main(  # pyright: ignore[reportUnknownMemberType,reportUnknownVariableType]
        [
            "grpc_tools.protoc",
            f"--proto_path={PROTO_DIR}",
            f"--python_out={PROTO_DIR}",
            f"--pyi_out={PROTO_DIR}",
            str(PROTO_FILE),
        ]
    )

    if result != 0:
        msg = f"protoc exited with code {result}"
        raise RuntimeError(msg)

    print(f"Compiled {PROTO_FILE.name} -> cast_channel_pb2.py, cast_channel_pb2.pyi")


if __name__ == "__main__":
    main()
