"""Observe released file-serving helpers over loopback HTTP; not a benchmark."""
import hashlib
import http.client
import json
import pathlib
import platform
import socket
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent
BINARY = ROOT / "target/debug/playscale-framework-probe"


def request(port, method="GET", headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
    try:
        connection.request(method, "/media", headers=headers or {})
        response = connection.getresponse()
        body = response.read()
        return {
            "status": response.status,
            "headers": {k.lower(): v for k, v in response.getheaders() if k.lower() in {
                "content-length", "content-range", "accept-ranges", "etag",
                "last-modified", "content-type", "content-encoding",
            }},
            "body_bytes": len(body),
            "body_sha256": hashlib.sha256(body).hexdigest(),
            "body_text": body.decode("utf-8", errors="replace"),
        }
    finally:
        connection.close()


def probe(framework, path):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    process = subprocess.Popen([str(BINARY), framework, str(port), str(path)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        for _ in range(100):
            if process.poll() is not None:
                raise RuntimeError(process.stderr.read().decode())
            try:
                baseline = request(port)
                break
            except (OSError, http.client.HTTPException):
                time.sleep(0.05)
        else:
            raise RuntimeError("server did not start")
        cases = [
            ("full", "GET", {}),
            ("head", "HEAD", {}),
            ("prefix", "GET", {"Range": "bytes=0-3"}),
            ("suffix", "GET", {"Range": "bytes=-3"}),
            ("open_end", "GET", {"Range": "bytes=4-"}),
            ("whole_range", "GET", {"Range": "bytes=0-9"}),
            ("beyond_end", "GET", {"Range": "bytes=4-99"}),
            ("unsatisfiable", "GET", {"Range": "bytes=99-100"}),
            ("malformed", "GET", {"Range": "not-a-range"}),
            ("multiple", "GET", {"Range": "bytes=0-1,4-5"}),
            ("stale_if_range", "GET", {"Range": "bytes=0-3", "If-Range": '"definitely-stale"'}),
            ("head_with_range", "HEAD", {"Range": "bytes=0-3"}),
        ]
        etag = baseline["headers"].get("etag")
        if etag:
            cases += [
                ("matching_if_range", "GET", {"Range": "bytes=0-3", "If-Range": etag}),
                ("matching_if_none_match", "GET", {"If-None-Match": etag}),
                ("not_modified_before_range", "GET", {"If-None-Match": etag, "Range": "bytes=99-100"}),
            ]
        return [{"case": name, "request_method": method, "request_headers": headers,
                 **request(port, method, headers)} for name, method, headers in cases]
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        process.stderr.close()


def main():
    result = {
        "scope": "Loopback HTTP/1.1, synthetic 10-byte and empty files; no TLS, Tailscale, Demuxe, performance, or disconnect tests",
        "platform": platform.platform(),
        "rustc": subprocess.check_output(["rustc", "+1.95.0", "--version"], text=True).strip(),
        "versions": {"axum": "0.8.9", "tower-http": "0.7.1", "poem": "3.1.12", "salvo": "1.0.1"},
        "results": {},
    }
    with tempfile.TemporaryDirectory(prefix="playscale-range-fixtures-") as directory:
        for label, content in [("ten_bytes", b"0123456789"), ("empty", b"")]:
            path = pathlib.Path(directory) / "fixture.bin"
            path.write_bytes(content)
            result["results"][label] = {framework: probe(framework, path)
                                         for framework in ["axum", "poem", "salvo"]}
    (ROOT / "results.json").write_text(json.dumps(result, indent=2) + "\n")
    for fixture, frameworks in result["results"].items():
        print(fixture)
        for framework, rows in frameworks.items():
            print(framework, [(r["case"], r["status"], r["headers"].get("content-range"), r["body_bytes"]) for r in rows])


if __name__ == "__main__":
    main()
