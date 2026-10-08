"""Generate small synthetic playback fixtures and a reproducible manifest."""
import argparse
import hashlib
import json
import pathlib
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=pathlib.Path, help='New output directory (never overwritten)')
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=False)

def ffmpeg(arguments, name):
    command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-n', *arguments, str(args.output / name)]
    subprocess.run(command, check=True)
    return command

common = ['-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=24', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '12']
commands = {}
commands['Pattern-360p.mp4'] = ffmpeg(common + ['-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-movflags', '+faststart'], 'Pattern-360p.mp4')
commands['Pattern-180p.mp4'] = ffmpeg(['-i', str(args.output / 'Pattern-360p.mp4'), '-vf', 'scale=320:180', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'copy', '-movflags', '+faststart'], 'Pattern-180p.mp4')
commands['Pattern-VP9.webm'] = ffmpeg(common + ['-c:v', 'libvpx-vp9', '-deadline', 'realtime', '-cpu-used', '8', '-crf', '36', '-b:v', '0', '-c:a', 'libopus'], 'Pattern-VP9.webm')
fixtures = []
for name, command in commands.items():
    path = args.output / name
    probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_format', '-show_streams', '-of', 'json', str(path)]))
    fixtures.append({'name': name, 'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'command': command, 'probe': probe})
manifest = {'synthetic': True, 'ffmpeg_version': subprocess.check_output(['ffmpeg', '-version'], text=True).splitlines()[0], 'fixtures': fixtures}
(args.output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(json.dumps([{'name': f['name'], 'bytes': f['bytes'], 'sha256': f['sha256']} for f in fixtures], indent=2))
