"""Opt-in disposable WsgiDAV + real Windows Credential Manager acceptance.

Install wsgidav and cheroot in your test environment, then run this script.
Uses only loopback, temporary files, random credentials, and an ephemeral port.
"""
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path


def serve():
    from cheroot.wsgi import Server
    from wsgidav.wsgidav_app import WsgiDAVApp
    app = WsgiDAVApp({
        "provider_mapping": {"/": sys.argv[3]},
        "simple_dc": {"user_mapping": {"*": {"acceptance": {
            "password": os.environ["NOTEBASE_ACCEPTANCE_PASSWORD"], "roles": []}}}},
        "http_authenticator": {"accept_basic": True, "accept_digest": False,
                               "default_to_digest": False},
        "verbose": 0,
    })
    Server(("127.0.0.1", int(sys.argv[2])), app).start()


def main():
    with tempfile.TemporaryDirectory(prefix="notebase-dav-") as root:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = dict(os.environ, NOTEBASE_ACCEPTANCE_PASSWORD=secrets.token_urlsafe(32),
                   NOTEBASE_ACCEPTANCE_DAV=f"http://127.0.0.1:{port}/")
        server = subprocess.Popen([sys.executable, __file__, "--serve", str(port), root],
                                  env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        try:
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError("Disposable WebDAV server failed to start")
                try:
                    urllib.request.urlopen(env["NOTEBASE_ACCEPTANCE_DAV"], timeout=1)
                except urllib.error.HTTPError as error:
                    if error.code == 401:
                        break
                except OSError:
                    pass
                time.sleep(0.1)
            else:
                raise RuntimeError("WebDAV readiness timeout")
            result = subprocess.run(["cargo", "test", "--manifest-path",
                "crates/notebase-core/Cargo.toml", "--test", "member3_acceptance",
                "--", "--ignored", "--nocapture"], env=env,
                cwd=Path(__file__).resolve().parent.parent)
            return result.returncode
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--serve":
        serve()
    else:
        sys.exit(main())
