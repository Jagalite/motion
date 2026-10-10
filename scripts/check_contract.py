"""Structural checks for the reviewed OpenAPI v2 contract.

    python3.13 -m venv /tmp/motion-contract && /tmp/motion-contract/bin/pip install -r scripts/ci/contract-requirements.txt
    /tmp/motion-contract/bin/python scripts/check_contract.py

YAML is the source: it must parse without duplicate keys, match the served JSON
form exactly, resolve every local $ref, and pass OpenAPI 3.1 validation. Which
operations the server routes is checked by tests/contract.rs.
"""
import json
import pathlib
import sys
from urllib.parse import unquote

import yaml
from openapi_spec_validator import validate

ROOT = pathlib.Path(__file__).resolve().parents[1]
YAML = ROOT / 'contracts/Motion_Server_API_v2.yaml'
JSON = ROOT / 'contracts/Motion_Server_API_v2.json'


class UniqueKeyLoader(yaml.SafeLoader):
    pass


def unique_mapping(loader, node, deep=False):
    seen = set()
    for key_node, _ in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in seen:
            raise ValueError(f'duplicate key {key!r} at line {key_node.start_mark.line + 1}')
        seen.add(key)
    return loader.construct_mapping(node, deep)


UniqueKeyLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, unique_mapping)


def references(value):
    if isinstance(value, dict):
        for key, item in value.items():
            if key == '$ref' and isinstance(item, str):
                yield item
            else:
                yield from references(item)
    elif isinstance(value, list):
        for item in value:
            yield from references(item)


def resolve(document, ref):
    """Resolve a same-document reference: a URI fragment holding a JSON Pointer
    (RFC 6901 section 6). `#` is the whole document."""
    if not ref.startswith('#'):
        raise ValueError(f'external reference {ref}')
    pointer = unquote(ref[1:])
    if pointer == '':
        return document
    if not pointer.startswith('/'):
        raise ValueError(f'not a JSON Pointer fragment: {ref}')
    node = document
    for part in pointer[1:].split('/'):
        part = part.replace('~1', '/').replace('~0', '~')
        if isinstance(node, list):
            if not part.isdigit() or (len(part) > 1 and part[0] == '0'):
                raise ValueError(f'invalid array index {part!r} in {ref}')
            node = node[int(part)]
        else:
            node = node[part]
    return node


def main():
    document = yaml.load(YAML.read_text(), Loader=UniqueKeyLoader)
    if json.loads(JSON.read_text()) != document:
        raise SystemExit(f'{JSON.name} differs from {YAML.name}; run ruby scripts/contract_json.rb')
    refs = list(references(document))
    broken = []
    for ref in sorted(set(refs)):
        try:
            resolve(document, ref)
        except (KeyError, IndexError, TypeError, ValueError):
            broken.append(ref)
    if broken:
        raise SystemExit('unresolved references: ' + ', '.join(broken))
    validate(document)
    operations = sum(1 for item in document['paths'].values() for key in item if key in
                     ('get', 'put', 'post', 'delete', 'patch', 'head', 'options', 'trace'))
    print(json.dumps({'paths': len(document['paths']), 'operations': operations,
                      'schemas': len(document['components']['schemas']), 'references': len(refs)}))


if __name__ == '__main__':
    sys.exit(main())
