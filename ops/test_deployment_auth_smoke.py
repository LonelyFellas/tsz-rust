import contextlib
import http.cookies
import http.server
import importlib.util
import io
import json
import pathlib
import socket
import ssl
import subprocess
import tempfile
import threading
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "ops/deployment_auth_smoke.py"


@contextlib.contextmanager
def serving(handler, tls_context=None):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    if tls_context is not None:
        server.socket = tls_context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


class DeploymentAuthSmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        cls.cert = pathlib.Path(cls.directory.name) / "cert.pem"
        cls.key = pathlib.Path(cls.directory.name) / "key.pem"
        config = pathlib.Path(cls.directory.name) / "openssl.cnf"
        config.write_text(
            "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n"
            "[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost\n"
            "basicConstraints=critical,CA:TRUE\n"
        )
        subprocess.run(
            ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
             "-days", "1", "-keyout", str(cls.key), "-out", str(cls.cert),
             "-config", str(config)],
            check=True, capture_output=True,
        )

    def setUp(self):
        self.assertTrue(SCRIPT.is_file(), "HTTPS auth smoke executable is missing")
        spec = importlib.util.spec_from_file_location("deployment_auth_smoke", SCRIPT)
        self.smoke = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.smoke)
        self.calls = []
        self.secure = True
        self.rotate = True
        self.login_status = 200
        self.redirect = False
        self.logout_clears = True
        self.duplicate_cookie = False
        self.login_error_cookie = False
        self.refresh_status = 200
        self.logout_status = 204
        self.problem_body = {"status": 401, "type": "about:blank", "title": "Unauthorized", "code": "invalid_refresh_token"}
        outer = self

        class API(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                body = json.dumps({"status": "ok" if self.path == "/healthz" else "ready"}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                cookie = http.cookies.SimpleCookie(self.headers.get("Cookie", ""))
                value = cookie["refresh_token"].value if "refresh_token" in cookie else None
                outer.calls.append((self.path, value))
                status, body, token = 200, {}, None
                if self.path.endswith("/login"):
                    if json.loads(data) != {"identifier": "fixture@example.invalid", "password": "test-only-secret"}:
                        status = 422
                    else:
                        status = outer.login_status
                        token = "first-token" if status == 200 or outer.login_error_cookie else None
                elif self.path.endswith("/refresh"):
                    if value is None:
                        status, body = 401, outer.problem_body
                    else:
                        status = outer.refresh_status
                        token = "second-token" if outer.rotate else "first-token"
                elif self.path.endswith("/logout"):
                    status = outer.logout_status
                    token = "" if outer.logout_clears and status == 204 else None
                else:
                    status = 404
                if outer.redirect and self.path.endswith("/login"):
                    status, token = 307, None
                self.send_response(status)
                self.send_header("Content-Type", "application/problem+json" if status == 401 else "application/json")
                if status == 307:
                    self.send_header("Location", "/api/v1/auth/login-again")
                if token is not None:
                    attributes = "; HttpOnly; SameSite=Lax; Path=/api/v1/auth"
                    if outer.secure:
                        attributes += "; Secure"
                    if token == "":
                        attributes += "; Max-Age=0"
                    if outer.duplicate_cookie and token:
                        self.send_header("Set-Cookie", "refresh_token=" + token + "; Secure; HttpOnly; SameSite=Lax; Path=/old")
                        self.send_header("Set-Cookie", "refresh_token=" + token + "; Path=/api/v1/auth")
                    else:
                        self.send_header("Set-Cookie", "refresh_token=" + token + attributes)
                self.end_headers()
                if status != 204:
                    self.wfile.write(json.dumps(body).encode())

        self.handler = API
        self.context = ssl.create_default_context(cafile=str(self.cert))

    def run_chain(self, context=None, hostname="localhost"):
        with serving(self.handler) as api:
            with self.smoke.candidate_tls(api.server_port, self.cert, self.key) as port:
                return self.smoke.run_smoke(
                    f"https://{hostname}:{port}", api.server_port,
                    {"identifier": "fixture@example.invalid", "password": "test-only-secret"},
                    context=context or self.context, loopback=True,
                )

    def test_secure_cookie_chain_rotates_and_logs_out_over_tls(self):
        self.run_chain()
        self.assertEqual(self.calls, [
            ("/api/v1/auth/refresh", None),
            ("/api/v1/auth/login", None),
            ("/api/v1/auth/refresh", "first-token"),
            ("/api/v1/auth/logout", "second-token"),
        ])

    def test_duplicate_cookie_attributes_cannot_be_combined_into_pass(self):
        self.duplicate_cookie = True
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain()
        self.assertNotIn(("/api/v1/auth/refresh", "first-token"), self.calls)

    def test_failed_login_with_cookie_still_logs_out(self):
        self.login_status = 500
        self.login_error_cookie = True
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain()
        self.assertEqual(self.calls[-1], ("/api/v1/auth/logout", "first-token"))

    def test_login_body_read_failure_still_cleans_up_received_cookie(self):
        tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls_context.load_cert_chain(self.cert, self.key)
        original_read = http.client.HTTPResponse.read

        def interrupted_read(response, *args, **kwargs):
            if "first-token" in response.getheader("Set-Cookie", ""):
                raise OSError("fixture read interrupted")
            return original_read(response, *args, **kwargs)

        with serving(self.handler) as health, serving(self.handler, tls_context) as api, \
             mock.patch.object(http.client.HTTPResponse, "read", interrupted_read):
            with self.assertRaises(self.smoke.SmokeFailure) as result:
                self.smoke.run_smoke(
                    f"https://localhost:{api.server_port}", health.server_port,
                    {"identifier": "fixture@example.invalid", "password": "test-only-secret"}, context=self.context,
                )
        self.assertEqual(str(result.exception), "invalid_response_or_transport")
        self.assertEqual(self.calls[-1], ("/api/v1/auth/logout", "first-token"))

    def test_cleanup_failure_preserves_original_failure(self):
        self.refresh_status = 500
        self.logout_status = 500
        with self.assertRaises(self.smoke.SmokeFailure) as result:
            self.run_chain()
        self.assertEqual(str(result.exception), "refresh_status_500")
        self.assertEqual(result.exception.cleanup_stage, "cleanup_logout_failed")

    def test_non_object_problem_body_returns_smoke_failure(self):
        for body in (None, []):
            with self.subTest(body=body):
                self.problem_body = body
                with self.assertRaises(self.smoke.SmokeFailure):
                    self.run_chain()

    def test_preflight_rejects_insecure_configuration(self):
        self.assertTrue(hasattr(self.smoke, "preflight"), "pre-stop TLS preflight missing")
        output = io.StringIO()
        with mock.patch.object(self.smoke.sys, "argv", [str(SCRIPT), "--mode", "preflight", "--owner", "11111111-1111-4111-8111-111111111111"]), \
             mock.patch.dict(self.smoke.os.environ, {"COOKIE_SECURE": "false"}), \
             contextlib.redirect_stdout(output):
            result = self.smoke.main()
        self.assertEqual(result, 1)
        self.assertEqual(json.loads(output.getvalue())["stage"], "cookie_secure_required")

    def test_recovery_without_original_remote_lock_cannot_report_success(self):
        output = io.StringIO()
        with mock.patch.object(self.smoke.sys, "argv", [str(SCRIPT), "--mode", "recovery", "--owner", "11111111-1111-4111-8111-111111111111"]), \
             mock.patch.object(self.smoke.sys, "stdin", io.StringIO('{"identifier":"fixture","password":"secret"}')), \
             mock.patch.object(pathlib.Path, "exists", return_value=False), \
             mock.patch.object(self.smoke, "run_smoke"), contextlib.redirect_stdout(output):
            result = self.smoke.main()
        self.assertEqual(result, 1)
        self.assertEqual(json.loads(output.getvalue())["result"], "FAIL")
        self.assertNotIn("secret", output.getvalue())

    def test_existing_mode_requires_matching_manifest_sha(self):
        output = io.StringIO()
        with mock.patch.object(self.smoke.sys, "argv", [str(SCRIPT), "--mode", "existing", "--owner", "11111111-1111-4111-8111-111111111111", "--expected-sha", "a" * 40]), \
             mock.patch.object(self.smoke.sys, "stdin", io.StringIO('{"identifier":"fixture","password":"secret"}')), \
             mock.patch.object(pathlib.Path, "exists", return_value=False), \
             mock.patch.object(pathlib.Path, "read_text", return_value=json.dumps({"source": {"git_sha": "b" * 40}})), \
             mock.patch.object(self.smoke, "run_smoke"), contextlib.redirect_stdout(output):
            result = self.smoke.main()
        self.assertEqual(result, 1)
        self.assertEqual(json.loads(output.getvalue())["stage"], "existing_sha_mismatch")

    def test_candidate_rejects_wrong_listener_pid_or_public_bind(self):
        self.assertTrue(hasattr(self.smoke, "check_candidate"), "candidate listener identity check missing")
        environment = b"BIND_IP=127.0.0.1\0DEPLOYMENT_SMOKE_ONLY=true\0PORT=18383\0COOKIE_SECURE=true\0"
        for listener in (
            'LISTEN 0 128 127.0.0.1:18383 0.0.0.0:* users:(("tsz-rust",pid=456,fd=7))',
            'LISTEN 0 128 0.0.0.0:18383 0.0.0.0:* users:(("tsz-rust",pid=123,fd=7))',
        ):
            with self.subTest(listener=listener), \
                 mock.patch.object(pathlib.Path, "read_text", return_value="123"), \
                 mock.patch.object(pathlib.Path, "read_bytes", return_value=environment), \
                 mock.patch.object(pathlib.Path, "samefile", return_value=True), \
                 mock.patch.object(self.smoke.subprocess, "run", side_effect=[
                     subprocess.CompletedProcess([], 3), subprocess.CompletedProcess([], 0, listener),
                 ]):
                with self.assertRaises(self.smoke.SmokeFailure):
                    self.smoke.check_candidate(pathlib.Path("/fixture-lock"))

    def test_candidate_accepts_recorded_loopback_process(self):
        self.assertTrue(hasattr(self.smoke, "check_candidate"), "candidate listener identity check missing")
        listener = 'LISTEN 0 128 127.0.0.1:18383 0.0.0.0:* users:(("tsz-rust",pid=123,fd=7))'
        with mock.patch.object(pathlib.Path, "read_text", return_value="123"), \
             mock.patch.object(pathlib.Path, "read_bytes", return_value=b"BIND_IP=127.0.0.1\0DEPLOYMENT_SMOKE_ONLY=true\0PORT=18383\0"), \
             mock.patch.object(pathlib.Path, "samefile", return_value=True), \
             mock.patch.object(self.smoke.subprocess, "run", side_effect=[
                 subprocess.CompletedProcess([], 3), subprocess.CompletedProcess([], 0, listener),
             ]):
            self.smoke.check_candidate(pathlib.Path("/fixture-lock"))

    def test_recovery_uses_https_cookie_chain_and_separate_local_health(self):
        with serving(self.handler) as api:
            with self.smoke.candidate_tls(api.server_port, self.cert, self.key) as port:
                self.smoke.run_smoke(
                    f"https://localhost:{port}", api.server_port,
                    {"identifier": "fixture@example.invalid", "password": "test-only-secret"},
                    context=self.context,
                )
        self.assertEqual(self.calls[-1], ("/api/v1/auth/logout", "second-token"))

    def test_insecure_cookie_fails_but_cleans_up_session(self):
        self.secure = False
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain()
        self.assertEqual(self.calls[-1], ("/api/v1/auth/logout", "first-token"))

    def test_refresh_without_rotation_fails_and_logs_out(self):
        self.rotate = False
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain()
        self.assertEqual(self.calls[-1], ("/api/v1/auth/logout", "first-token"))

    def test_logout_must_remove_cookie(self):
        self.logout_clears = False
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain()

    def test_login_failure_is_not_retried(self):
        self.login_status = 401
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain()
        self.assertEqual(len(self.calls), 2)

    def test_redirect_is_not_followed(self):
        self.redirect = True
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain()
        self.assertEqual(len(self.calls), 2)

    def test_untrusted_certificate_fails_before_auth_request(self):
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain(context=ssl.create_default_context())
        self.assertEqual(self.calls, [])

    def test_wrong_hostname_fails_before_auth_request(self):
        with self.assertRaises(self.smoke.SmokeFailure):
            self.run_chain(hostname="wrong.invalid")
        self.assertEqual(self.calls, [])

    def test_http_cannot_be_used_for_auth(self):
        with self.assertRaises(self.smoke.SmokeFailure):
            self.smoke.run_smoke("http://localhost", 1, {})
        self.assertEqual(self.calls, [])

    def test_candidate_tls_is_loopback_only_and_removed_on_failure(self):
        with serving(self.handler) as api:
            with self.assertRaisesRegex(RuntimeError, "test failure"):
                with self.smoke.candidate_tls(api.server_port, self.cert, self.key) as port:
                    with socket.create_connection(("127.0.0.1", port), timeout=2):
                        pass
                    raise RuntimeError("test failure")
            with self.assertRaises(OSError):
                socket.create_connection(("127.0.0.1", port), timeout=1)

    def test_proxy_does_not_forward_non_auth_paths(self):
        import http.client
        with serving(self.handler) as api:
            with self.smoke.candidate_tls(api.server_port, self.cert, self.key) as port:
                connection = http.client.HTTPSConnection("localhost", port, context=self.context, timeout=3)
                connection.request("POST", "/api/v1/admin/auth/login", body=b"{}")
                with connection.getresponse() as response:
                    self.assertEqual(response.status, 404)
                connection.close()
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
