"""Public v2 HTTP helper for current-server acceptance; never sends legacy calls."""
import http.client
import json
import uuid
from smoke import request


class Client:
    def __init__(self, port, token):
        self.port = port
        self.auth = {'Authorization': 'Bearer ' + token}
        self.requests = []

    def call(self, method, path, body=None, expected=200, headers=None, raw=False):
        if not path.startswith('/api/v2/'):
            raise ValueError(f'Current-server request outside v2: {path}')
        extra = dict(self.auth, **(headers or {}))
        if method == 'POST':
            extra.setdefault('Idempotency-Key', str(uuid.uuid4()))
        self.requests.append({'method': method, 'path': path})
        if raw:
            connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=10)
            try:
                connection.request(method, path, body, extra)
                response = connection.getresponse()
                status, result_headers, payload = response.status, dict(response.getheaders()), response.read()
            finally:
                connection.close()
        else:
            status, result_headers, payload = request(self.port, method, path, body, extra)
        assert status == expected, (method, path, status, payload[:1000])
        kind = result_headers.get('content-type', '').split(';')[0]
        result = json.loads(payload) if payload and kind in ('application/json', 'application/problem+json') else payload
        return result, result_headers

    def json(self, method, path, body=None, expected=200, headers=None):
        return self.call(method, path, body, expected, headers)[0]

    def etag(self, path):
        return self.call('GET', path)[1]['etag']
