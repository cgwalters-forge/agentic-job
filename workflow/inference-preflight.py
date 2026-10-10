"""Check the runner's TCP control before sandbox setup; send no credentials."""

import socket
import sys
import tomllib
from urllib.parse import urlsplit


def check(config, connect=socket.create_connection):
    url = config.get("inference", {}).get("url")
    if not url or config.get("agent", {}).get("name") == "fake":
        return
    endpoint = urlsplit(url)
    if endpoint.scheme not in ("http", "https") or not endpoint.hostname:
        raise ValueError("inference-url must be an HTTP(S) URL")
    port = endpoint.port or (443 if endpoint.scheme == "https" else 80)
    try:
        with connect((endpoint.hostname, port), timeout=10):
            pass
    except OSError as error:
        raise RuntimeError(
            f"control failed: the runner cannot reach the inference proxy at {url}: "
            "join its network in a step before secure-host, or use a runner on it"
        ) from error


if __name__ == "__main__":
    try:
        with open(sys.argv[1], "rb") as source:
            check(tomllib.load(source))
    except (OSError, ValueError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(1)
