"""Exercise logical catalog and artwork APIs through a real, isolated HTTP server."""
import binascii
import json
import pathlib
import struct
import subprocess
import tempfile
import zlib
from smoke import BINARY, free_port, request, wait_for, stop


def png():
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', binascii.crc32(kind + data) & 0xffffffff)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 2, 2, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00\x00\xff\x00' * 2)) + chunk(b'IEND', b'')


def main():
    from v2_smoke_support import Client
    checks = []
    with tempfile.TemporaryDirectory(prefix='motion-catalog-') as directory:
        base = pathlib.Path(directory)
        media = base / 'media'
        media.mkdir()
        subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=160x90:rate=12', '-t', '1', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', str(media / 'Episode.mp4')], check=True)
        port = free_port()
        command = [str(BINARY), '--listen', f'127.0.0.1:{port}', '--data-dir', str(base / 'state'), '--library', str(media)]
        with (base / 'server.log').open('wb') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: request(port, 'GET', '/api/v2/system/health')[0] == 200)
                client = Client(port, (base / 'state/admin-token').read_text().strip())
                api = client.json
                def discovered():
                    rows = api('GET', '/api/v2/catalog/items')['items']
                    return rows[0] if rows else None
                item = wait_for(discovered)
                item_path = '/api/v2/catalog/items/' + item['id']
                series = api('POST', '/api/v2/catalog/items', {'title': 'Test Show', 'kind': 'series', 'library_ids': []}, 201)
                # v2 models containment as an explicit, revision-fenced relationship.
                relation_path = item_path + '/relationships'
                relation = {'source_item_id': item['id'], 'target_item_id': series['id'], 'kind': 'part_of', 'position': 1}
                headers = {'If-Match': client.etag(item_path)}
                edge = api('POST', relation_path, relation, 201, headers)
                api('POST', relation_path, dict(relation, position=2), 412, headers)
                assert api('GET', relation_path)['items'][0]['id'] == edge['id']
                checks.append('explicit_hierarchy_and_revision_conflicts')
                metadata_path = item_path + '/metadata'
                api('PUT', metadata_path + '/contributions/catabolic', {
                    'values': {'title': 'Imported Episode', 'release_year': 2024},
                    'tags': ['Test'], 'excluded_tags': [], 'locked_fields': [],
                }, headers={'If-Match': client.etag(metadata_path)})
                metadata = api('GET', metadata_path)
                assert metadata['values']['title'] == 'Imported Episode'
                assert metadata['field_sources']['title'] == ['catabolic']
                assert api('GET', item_path)['title'] == 'Imported Episode'
                checks.append('attributed_metadata_and_catalog_projection')
                edition = api('POST', item_path + '/editions', {'label': 'Broadcast cut', 'kind': 'default'}, 201,
                              {'If-Match': client.etag(item_path)})
                assert edition['id'] in [e['id'] for e in api('GET', item_path + '/editions')['items']]
                assert edition['timeline_ids'][0] in [t['id'] for t in api('GET', item_path + '/timelines')['items']]
                checks.append('edition_creation_and_explicit_timeline_identity')
                artwork, _ = client.call('POST', item_path + '/artwork', png(), 201,
                                         {'Content-Type': 'application/octet-stream'}, raw=True)
                asset = artwork['id']
                api('PUT', item_path + '/artwork/selection', {'role': 'poster', 'asset_id': asset},
                    headers={'If-Match': client.etag(item_path)})
                content_path = '/api/v2/media/assets/' + asset + '/content?revision=' + artwork['revision']
                payload, headers = client.call('GET', content_path)
                assert payload.startswith(b'\x89PNG\r\n\x1a\n') and headers['content-type'] == 'image/png'
                assert struct.unpack('>II', payload[16:24]) == (2, 2)
                client.call('GET', content_path, expected=304, headers={'If-None-Match': headers['etag']})
                checks.append('artwork_upload_pin_canonical_bytes_and_etag')
                spec = api('GET', '/api/v2/openapi.json')
                operations = [operation['operationId'] for path in spec['paths'].values() for method, operation in path.items() if method in ['get', 'put', 'post', 'head', 'delete']]
                assert len(operations) == len(set(operations)), operations
                def refs(value):
                    if isinstance(value, dict):
                        if '$ref' in value:
                            node = spec
                            assert value['$ref'].startswith('#/')
                            for key in value['$ref'][2:].split('/'):
                                node = node[key.replace('~1', '/').replace('~0', '~')]
                        for child in value.values(): refs(child)
                    elif isinstance(value, list):
                        for child in value: refs(child)
                refs(spec)
                assert spec['paths']['/api/v2/catalog/items/{item_id}/artwork']['post']['requestBody']['content']['application/octet-stream']['schema']['format'] == 'binary'
                checks.append('openapi_references_unique_operations_and_binary_upload')
                stop(process)
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                wait_for(lambda: request(port, 'GET', '/api/v2/system/health')[0] == 200)
                assert api('GET', relation_path)['items'][0]['id'] == edge['id']
                assert api('GET', item_path)['artwork_id'] == asset
                assert any(e['id'] == edition['id'] and e['label'] == 'Broadcast cut' for e in api('GET', item_path + '/editions')['items'])
                assert api('GET', metadata_path)['values']['title'] == 'Imported Episode'
                checks.append('restart_preserves_relationships_editions_metadata_and_artwork')
            finally:
                stop(process)
    print(json.dumps({'passed': len(checks), 'checks': checks, 'api_version': 'v2'}, indent=2))


if __name__ == '__main__':
    main()
