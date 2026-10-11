import io
import json
import unittest
import urllib.error
from client import catalog, NoRedirect


class Pages:
    def __init__(self, *pages):
        self.pages = iter(pages)
        self.requests = []

    def open(self, request, timeout):
        self.requests.append(request)
        return io.BytesIO(json.dumps(next(self.pages)).encode())


class CatalogTests(unittest.TestCase):
    def test_authenticated_cursor_pagination(self):
        pages = Pages({'items': [{'id': 'a'}], 'next_cursor': 'opaque+/='},
                      {'items': [{'id': 'b'}], 'next_cursor': None})
        self.assertEqual(list(catalog('https://motion.test/', 'secret', pages)), [{'id': 'a'}, {'id': 'b'}])
        self.assertEqual(pages.requests[1].full_url, 'https://motion.test/api/v2/catalog/items?limit=100&cursor=opaque%2B%2F%3D')
        self.assertTrue(all(r.get_header('Authorization') == 'Bearer secret' for r in pages.requests))

    def test_repeated_cursor_fails(self):
        pages = Pages(*[{'items': [], 'next_cursor': 'same'}] * 2)
        with self.assertRaisesRegex(ValueError, 'repeated'):
            list(catalog('https://motion.test', 'secret', pages))

    def test_origin_and_credential_validation(self):
        for origin in ['file:///tmp/catalog', 'https://user:pass@motion.test', 'https://motion.test/path', 'https://motion.test?token=x']:
            with self.assertRaises(ValueError):
                list(catalog(origin, 'secret'))
        with self.assertRaises(ValueError):
            list(catalog('https://motion.test', None))
        self.assertIsNone(NoRedirect().redirect_request(None, None, 302, '', {}, 'https://other.test'))


if __name__ == '__main__':
    unittest.main()
