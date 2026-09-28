import argparse
import contextlib
import http.client
import http.cookiejar
import http.cookies
import http.server
import json
import pathlib
import re
import socket
import ssl
import subprocess
import sys
import threading
import urllib.error
import urllib.parse
import urllib.request
import uuid


HOST = "test.tianshengzhi.com"
AUTH_PATH = "/api/v1/auth"
TIMEOUT = 10


class SmokeFailure(Exception):
    pass


def require(condition, stage):
    if not condition:
        raise SmokeFailure(stage)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


class LoopbackHTTPS(urllib.request.HTTPSHandler):
    def https_open(self, request):
        def connection(host, **kwargs):
            client = http.client.HTTPSConnection(host, **kwargs)
            # Keep the URL hostname for SNI and certificate verification, changing only the TCP destination.
            client._create_connection = lambda address, timeout, source_address=None: socket.create_connection(
                ("127.0.0.1", address[1]), timeout, source_address
            )
            return client

        return self.do_open(connection, request, context=self._context)


@contextlib.contextmanager
def candidate_tls(upstream_port, certificate, private_key):
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certificate, private_key)

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def setup(self):
            super().setup()
            self.connection.settimeout(TIMEOUT)

        def do_POST(self):
            if self.path not in {f"{AUTH_PATH}/{name}" for name in ("login", "refresh", "logout")}:
                self.send_error(404)
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 <= length <= 65536 or self.headers.get("Transfer-Encoding"):
                    self.send_error(400)
                    return
                body = self.rfile.read(length)
                headers = {"Content-Type": "application/json"}
                if self.headers.get("Cookie"):
                    headers["Cookie"] = self.headers["Cookie"]
                with contextlib.closing(http.client.HTTPConnection("127.0.0.1", upstream_port, timeout=TIMEOUT)) as upstream:
                    upstream.request("POST", self.path, body=body, headers=headers)
                    with upstream.getresponse() as response:
                        payload = response.read(1048577)
                        if len(payload) > 1048576:
                            self.send_error(502)
                            return
                        self.send_response(response.status)
                        for name, value in response.getheaders():
                            if name.lower() in {"content-type", "set-cookie", "location"}:
                                self.send_header(name, value)
                        self.send_header("Content-Length", str(len(payload)))
                        self.end_headers()
                        self.wfile.write(payload)
            except (OSError, ValueError, http.client.HTTPException):
                self.send_error(502)

    class Server(http.server.ThreadingHTTPServer):
        daemon_threads = True

        def get_request(self):
            connection, address = super().get_request()
            connection.settimeout(TIMEOUT)
            try:
                return context.wrap_socket(connection, server_side=True), address
            except BaseException:
                connection.close()
                raise

        def handle_error(self, request, client_address):
            pass

    server = Server(("127.0.0.1", 0), Proxy)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_port
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def run_smoke(base_url, health_port, credentials, *, context=None, loopback=False):
    url = urllib.parse.urlsplit(base_url)
    require(url.scheme == "https" and url.hostname and not url.username and not url.password
            and url.path in ("", "/") and not url.query and not url.fragment, "https_origin_required")
    context = context or ssl.create_default_context()
    require(context.check_hostname and context.verify_mode == ssl.CERT_REQUIRED, "tls_verification_required")
    jar = http.cookiejar.CookieJar()
    handler = LoopbackHTTPS(context=context) if loopback else urllib.request.HTTPSHandler(context=context)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), handler,
                                        urllib.request.HTTPCookieProcessor(jar), NoRedirect())
    authenticated = False

    def request(endpoint, payload=None):
        body = json.dumps(payload or {}).encode()
        req = urllib.request.Request(base_url.rstrip("/") + AUTH_PATH + endpoint, data=body,
                                     headers={"Content-Type": "application/json"}, method="POST")
        try:
            response = opener.open(req, timeout=TIMEOUT)
        except urllib.error.HTTPError as error:
            response = error
        except (OSError, urllib.error.URLError, http.client.HTTPException):
            raise SmokeFailure("auth_transport_failed") from None
        with response:
            return response.status, response.headers, response.read(1048576)

    def refresh_cookie(headers):
        cookies = http.cookies.SimpleCookie()
        for value in headers.get_all("Set-Cookie", []):
            cookies.load(value)
        require("refresh_token" in cookies, "refresh_cookie_missing")
        cookie = cookies["refresh_token"]
        require(cookie["secure"] and cookie["httponly"] and cookie["samesite"].lower() == "lax"
                and cookie["path"] == AUTH_PATH and cookie.value, "refresh_cookie_attributes")
        require(any(item.name == "refresh_token" and item.value == cookie.value for item in jar),
                "refresh_cookie_rejected")
        return cookie.value

    try:
        for endpoint, expected in (("healthz", "ok"), ("readyz", "ready")):
            with contextlib.closing(http.client.HTTPConnection("127.0.0.1", health_port, timeout=TIMEOUT)) as client:
                client.request("GET", "/" + endpoint)
                with client.getresponse() as response:
                    require(response.status == 200 and json.loads(response.read(65536)) == {"status": expected}, endpoint)
        status, headers, body = request("/refresh")
        require(status == 401 and headers.get_content_type() == "application/problem+json", "anonymous_refresh")
        problem = json.loads(body)
        require(problem.get("status") == 401 and problem.get("code") == "invalid_refresh_token"
                and problem.get("type") and problem.get("title"), "anonymous_refresh_problem")
        status, headers, _ = request("/login", credentials)
        require(status == 200, f"login_status_{status}")
        authenticated = True
        previous = refresh_cookie(headers)
        status, headers, _ = request("/refresh")
        require(status == 200, f"refresh_status_{status}")
        require(refresh_cookie(headers) != previous, "refresh_not_rotated")
        status, _, _ = request("/logout")
        require(status == 204, f"logout_status_{status}")
        require(not any(item.name == "refresh_token" for item in jar), "logout_cookie_not_cleared")
        authenticated = False
    except (ValueError, OSError, http.client.HTTPException, http.cookies.CookieError):
        raise SmokeFailure("invalid_response_or_transport") from None
    finally:
        if authenticated:
            if any(item.name == "refresh_token" for item in jar):
                status, _, _ = request("/logout")
                require(status == 204 and not any(item.name == "refresh_token" for item in jar), "cleanup_logout_failed")
            else:
                raise SmokeFailure("cleanup_cookie_unavailable")


def check_candidate(lock):
    process = subprocess.run(
        ["systemctl", "is-active", "--quiet", "tsz-rust"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=TIMEOUT, check=False,
    )
    require(process.returncode == 3, "formal_service_not_stopped")
    pid = int((lock / "candidate_pid").read_text().strip())
    require(pid > 0, "invalid_candidate_pid")
    process_path = pathlib.Path(f"/proc/{pid}")
    require((process_path / "exe").samefile("/opt/tsz-rust/target/release/tsz-rust"), "candidate_binary_mismatch")
    environment = (process_path / "environ").read_bytes().split(b"\0")
    require(all(value in environment for value in (
        b"BIND_IP=127.0.0.1", b"DEPLOYMENT_SMOKE_ONLY=true", b"PORT=18383",
    )), "candidate_environment")
    secure_values = [value for value in environment if value.startswith(b"COOKIE_SECURE=")]
    require(not secure_values or secure_values == [b"COOKIE_SECURE=true"], "candidate_cookie_secure")
    result = subprocess.run(
        ["ss", "-H", "-ltnp", "sport = :18383"], stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, text=True, timeout=TIMEOUT, check=False,
    )
    rows = result.stdout.splitlines()
    require(result.returncode == 0 and len(rows) == 1, "candidate_listener")
    fields = rows[0].split()
    require(len(fields) >= 6 and fields[3] == "127.0.0.1:18383"
            and f"pid={pid}," in " ".join(fields[5:]), "candidate_listener")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("candidate", "recovery", "existing"), required=True)
    parser.add_argument("--owner", required=True)
    parser.add_argument("--expected-sha")
    args = parser.parse_args()
    try:
        require(str(uuid.UUID(args.owner)) == args.owner, "invalid_owner")
        lock = pathlib.Path("/opt/tsz-rust/deploy.lock")
        owner_file = lock / "owner"
        locked = lock.exists()
        if args.mode == "existing":
            require(not locked, "existing_remote_lock_present")
            require(re.fullmatch(r"[0-9a-f]{40}", args.expected_sha or ""), "expected_sha_required")
            manifest = json.loads(pathlib.Path("/opt/tsz-deploy-manifests/api.json").read_text())
            require(manifest.get("source", {}).get("git_sha") == args.expected_sha, "existing_sha_mismatch")
        else:
            require(locked, "deployment_lock_missing")
            require(owner_file.read_text().strip() == args.owner, "owner_mismatch")
        credentials = json.load(sys.stdin)
        require(isinstance(credentials, dict) and set(credentials) == {"identifier", "password"}
                and all(isinstance(v, str) and v for v in credentials.values()), "invalid_credentials_input")
        if args.mode == "candidate":
            check_candidate(lock)
            cert_dir = pathlib.Path("/etc/letsencrypt/live") / HOST
            with candidate_tls(18383, cert_dir / "fullchain.pem", cert_dir / "privkey.pem") as port:
                run_smoke(f"https://{HOST}:{port}", 18383, credentials, loopback=True)
        else:
            run_smoke(f"https://{HOST}", 8383, credentials)
        if locked:
            require(owner_file.read_text().strip() == args.owner, "owner_changed")
        else:
            require(not lock.exists(), "owner_changed")
        print(json.dumps({"mode": args.mode, "health": "PASS", "ready": "PASS", "auth": "PASS"}))
        return 0
    except SmokeFailure as error:
        print(json.dumps({"result": "FAIL", "stage": str(error)}))
    except (OSError, ValueError, TypeError, subprocess.TimeoutExpired):
        print(json.dumps({"result": "FAIL", "stage": "precondition_failed"}))
    return 1


if __name__ == "__main__":
    sys.exit(main())
