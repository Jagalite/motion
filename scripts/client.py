"""Print the authorized v2 catalog as JSON lines (standard library only)."""
import argparse
import json
import os
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    # Never forward a bearer credential to a redirected origin.
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def catalog(origin, token, opener=None):
    url = urllib.parse.urlsplit(origin)
    if url.scheme not in ('http', 'https') or not url.netloc or url.username or url.password or url.query or url.fragment or url.path not in ('', '/'):
        raise ValueError('origin must be an HTTP(S) origin without credentials, path, query or fragment')
    if not token or '\n' in token or '\r' in token:
        raise ValueError('MOTION_ACCESS_TOKEN must contain a valid credential')
    opener = opener or urllib.request.build_opener(NoRedirect())
    cursor = None
    seen = set()
    while True:
        query = {'limit': 100}
        if cursor is not None:
            query['cursor'] = cursor
        request = urllib.request.Request(origin.rstrip('/') + '/api/v2/catalog/items?' + urllib.parse.urlencode(query),
                                         headers={'Accept': 'application/json', 'Authorization': 'Bearer ' + token})
        with opener.open(request, timeout=30) as response:
            page = json.load(response)
        yield from page['items']
        cursor = page['next_cursor']
        if cursor is None:
            return
        if not isinstance(cursor, str) or not cursor or cursor in seen:
            raise ValueError('server returned an invalid or repeated catalog cursor')
        seen.add(cursor)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('origin')
    args = parser.parse_args()
    try:
        for item in catalog(args.origin, os.environ.get('MOTION_ACCESS_TOKEN')):
            print(json.dumps(item))
    except (ValueError, urllib.error.URLError) as error:
        parser.exit(1, f'catalog: {error}\n')


if __name__ == '__main__':
    main()
