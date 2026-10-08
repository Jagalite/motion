"""Exercise logical catalog and artwork APIs through a real, isolated HTTP server."""
import binascii
import json
import pathlib
import struct
import subprocess
import tempfile
import zlib
from smoke import BINARY, free_port, request, wait_for, stop
import http.client


def png():
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', binascii.crc32(kind + data) & 0xffffffff)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 2, 2, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00\x00\xff\x00' * 2)) + chunk(b'IEND', b'')


def main():
    checks = []
    with tempfile.TemporaryDirectory(prefix='playscale-catalog-') as directory:
        base = pathlib.Path(directory)
        media = base / 'media'
        media.mkdir()
        subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=160x90:rate=12', '-t', '1', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', str(media / 'Episode.mp4')], check=True)
        port = free_port()
        command = [str(BINARY), '--listen', f'127.0.0.1:{port}', '--data-dir', str(base / 'state'), '--library', str(media)]
        with (base / 'server.log').open('wb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: request(port, 'GET', '/health')[0] == 200)
                auth = {'Authorization': 'Bearer ' + (base / 'state/admin-token').read_text().strip()}
                def api(method, path, body=None, expected=200):
                    status, _, payload = request(port, method, '/api/v1' + path, body, auth)
                    assert status == expected, (method, path, status, payload)
                    return json.loads(payload)
                def discovered():
                    rows = api('GET', '/items')['items']
                    return rows[0] if rows else None
                item = wait_for(discovered)
                series = api('POST', '/catalog/items', {'title': 'Test Show', 'media_type': 'series'}, 201)
                season = api('POST', '/catalog/items', {'title': 'Season 1', 'media_type': 'season', 'parent_id': series['id'], 'number': 1}, 201)
                structure = {'expected_revision': 0, 'media_type': 'episode', 'parent_id': season['id'], 'number': 1}
                api('PUT', f'/items/{item["id"]}/structure', structure)
                api('PUT', f'/items/{item["id"]}/structure', structure, 409)
                children = api('GET', f'/catalog/items?parent_id={season["id"]}')
                assert children['total'] == 1 and children['items'][0]['id'] == item['id']
                checks.append('hierarchy_and_revision_conflicts')
                api('PUT', f'/items/{item["id"]}/metadata/catabolic', {'expected_revision': 0, 'external_id': 'episode-1', 'values': {'title': 'Imported Episode', 'release_date': '2024-02-29', 'cast': [{'name': 'Test Actor', 'role': 'Host'}]}, 'tags': ['Test']})
                assert api('GET', '/catalog/items?source=catabolic&external_id=episode-1')['items'][0]['id'] == item['id']
                checks.append('metadata_external_identity_lookup')
                edition = api('POST', f'/items/{item["id"]}/editions', {'label': 'Broadcast'}, 201)
                api('PUT', f'/files/{item["file_id"]}/edition', {'expected_edition_id': item['edition_id'], 'edition_id': edition['id']})
                api('PUT', f'/editions/{edition["id"]}', {'expected_revision': 1, 'label': 'Broadcast cut'})
                checks.append('edition_assignment_and_rename')
                image = png()
                connection = http.client.HTTPConnection('127.0.0.1', port, timeout=10)
                try:
                    connection.request('PUT', f'/api/v1/items/{item["id"]}/artwork/poster/catabolic?expected_revision=0', image, dict(auth, **{'Content-Type': 'application/octet-stream'}))
                    response = connection.getresponse()
                    payload = response.read()
                    assert response.status == 200, payload
                    artwork = json.loads(payload)
                finally:
                    connection.close()
                asset = artwork['selections'][0]['asset_id']
                api('PUT', f'/items/{item["id"]}/artwork-selection/poster', {'expected_revision': 0, 'asset_id': asset})
                status, headers, payload = request(port, 'GET', f'/api/v1/artwork/{asset}/content')
                assert status == 200 and payload == image and headers['content-type'] == 'image/png'
                assert request(port, 'GET', f'/api/v1/artwork/{asset}/content', headers={'If-None-Match': headers['etag']})[0] == 304
                checks.append('artwork_upload_pin_bytes_and_etag')
                spec = api('GET', '/openapi.json')
                operations = [operation['operationId'] for path in spec['paths'].values() for method, operation in path.items() if method in ['get', 'put', 'post', 'head']]
                assert len(operations) == len(set(operations)), operations
                def refs(value):
                    if isinstance(value, dict):
                        if '$ref' in value:
                            node = spec
                            assert value['$ref'].startswith('#/')
                            for key in value['$ref'][2:].split('/'):
                                node = node[key.replace('~1', '/').replace('~0', '~')]
                        for child in value.values():
                            refs(child)
                    elif isinstance(value, list):
                        for child in value:
                            refs(child)
                refs(spec)
                assert spec['paths']['/api/v1/items/{id}/artwork/{role}/{source}']['put']['requestBody']['content']['application/octet-stream']['schema']['format'] == 'binary'
                checks.append('openapi_references_unique_operations_and_binary_upload')
                stop(process)
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                wait_for(lambda: request(port, 'GET', '/health')[0] == 200)
                assert api('GET', f'/catalog/items/{item["id"]}')['parent_id'] == season['id']
                assert api('GET', f'/items/{item["id"]}/artwork')['selections'][0]['asset_id'] == asset
                assert api('GET', f'/items/{item["id"]}')['edition_label'] == 'Broadcast cut'
                checks.append('restart_preserves_structure_editions_and_artwork')
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks}, indent=2))


if __name__ == '__main__':
    main()
