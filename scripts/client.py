"""Independent, standard-library-only catalog client."""
import argparse,json,urllib.request
p=argparse.ArgumentParser();p.add_argument('origin');args=p.parse_args()
offset=0
while True:
    with urllib.request.urlopen(args.origin.rstrip('/')+f'/api/v1/items?limit=100&offset={offset}') as response:page=json.load(response)
    for item in page['items']:print(json.dumps({k:item[k] for k in ['id','file_id','title','available','media_url']}))
    offset+=len(page['items'])
    if not page['items'] or offset>=page['total']:break
