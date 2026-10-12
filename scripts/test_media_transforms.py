"""Prove the bundled FFmpeg's HDR tone-mapping and libass subtitle filters.

Runs real filter graphs against generated fixtures with exact code values and
checks pixels, not just exit status. Set MOTION_MEDIA_TOOLS to the directory
holding the built ffmpeg/ffprobe (for example <build>/install/bin). Without it
every test is skipped, which is not a pass.
"""
import json
import math
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

TOOLS = os.environ.get('MOTION_MEDIA_TOOLS')
WIDTH, HEIGHT, BAND = 256, 64, 64
# Patch luminances in cd/m2: black, dim, diffuse white and a 1000-nit highlight.
NITS = (0, 10, 100, 1000)
REFERENCE_WHITE = 100  # zscale npl: linear 1.0 is 100 cd/m2.
PEAK_NITS = 1000       # The tone-map peak Motion derives from mastering metadata.


def tone_map_filter(transfer):
    """The selected SDR route: linearize with zimg, Hable tone map, BT.709 encode.

    Input and intermediate properties are explicit: sources often carry
    incomplete tags, and zimg finds no conversion path from untagged frames.
    """
    return ','.join([
        f'zscale=tin={transfer}:pin=bt2020:min=bt2020nc:rin=limited'
        f':t=linear:npl={REFERENCE_WHITE}:p=bt2020:m=bt2020nc:r=full',
        'format=gbrpf32le',
        'zscale=p=bt709',
        f'tonemap=tonemap=hable:desat=0:peak={PEAK_NITS / REFERENCE_WHITE:g}',
        'zscale=t=bt709:m=bt709:r=limited',
        'format=yuv420p'])


def pq_code(nits):
    """SMPTE ST 2084 inverse EOTF as a 10-bit limited-range code value."""
    m1, m2 = 2610 / 16384, 2523 / 4096 * 128
    c1, c2, c3 = 3424 / 4096, 2413 / 4096 * 32, 2392 / 4096 * 32
    y = (nits / 10000) ** m1
    return round(64 + 876 * ((c1 + c2 * y) / (1 + c3 * y)) ** m2)


def hable(x):
    a, b, c, d, e, f = 0.15, 0.50, 0.10, 0.20, 0.02, 0.30
    return (x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * f) - e / f


def expected_sdr_luma(nits):
    """8-bit limited BT.709 luma FFmpeg's Hable tonemap should produce.

    zimg encodes display-referred BT.709 with the inverse BT.1886 EOTF
    (gamma 1/2.4), not the camera OETF.
    """
    peak = PEAK_NITS / REFERENCE_WHITE
    signal = nits / REFERENCE_WHITE
    linear = 0 if signal == 0 else hable(signal) / hable(peak)
    return 16 + 219 * linear ** (1 / 2.4)


def probe(path):
    output = subprocess.check_output([str(Path(TOOLS) / 'ffprobe'), '-v', 'error', '-show_streams',
                                      '-of', 'json', str(path)], text=True)
    return json.loads(output)['streams']


@unittest.skipUnless(TOOLS, 'MOTION_MEDIA_TOOLS is not set')
class MediaTransformTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)

    def ffmpeg(self, *args, timeout=120):
        subprocess.run([str(Path(TOOLS) / 'ffmpeg'), '-hide_banner', '-v', 'error', '-y', *args],
                       check=True, timeout=timeout, cwd=self.work)

    def frame(self, media, seconds=0.0):
        """Decode one frame as 8-bit yuv420p planes."""
        raw = subprocess.check_output(
            [str(Path(TOOLS) / 'ffmpeg'), '-v', 'error', '-ss', f'{seconds:.3f}', '-i', str(media),
             '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'yuv420p', '-'], timeout=60)
        luma = WIDTH * HEIGHT
        self.assertEqual(len(raw), luma * 3 // 2)
        return raw[:luma], raw[luma:luma * 5 // 4], raw[luma * 5 // 4:]

    @staticmethod
    def mean(plane, width, x0, x1, y0, y1):
        values = [plane[y * width + x] for y in range(y0, y1) for x in range(x0, x1)]
        return sum(values) / len(values)

    def band_means(self, media):
        y, u, v = self.frame(media)
        # Sample band interiors to avoid chroma subsampling at the edges.
        lumas = [self.mean(y, WIDTH, i * BAND + 8, (i + 1) * BAND - 8, 8, HEIGHT - 8)
                 for i in range(len(NITS))]
        chromas = [(self.mean(u, WIDTH // 2, i * BAND // 2 + 4, (i + 1) * BAND // 2 - 4, 4, HEIGHT // 2 - 4),
                    self.mean(v, WIDTH // 2, i * BAND // 2 + 4, (i + 1) * BAND // 2 - 4, 4, HEIGHT // 2 - 4))
                   for i in range(len(NITS))]
        return lumas, chromas

    def hdr10_fixture(self):
        """Lossless 10-bit PQ/BT.2020 neutral bands with exact code values."""
        raw = self.work / 'bands.yuv'
        row = b''.join(pq_code(nits).to_bytes(2, 'little') * BAND for nits in NITS)
        chroma = (512).to_bytes(2, 'little') * (WIDTH // 2 * HEIGHT // 2)
        raw.write_bytes(row * HEIGHT + chroma * 2)
        fixture = self.work / 'hdr10.mkv'
        self.ffmpeg('-f', 'rawvideo', '-pixel_format', 'yuv420p10le', '-video_size', f'{WIDTH}x{HEIGHT}',
                    '-framerate', '24', '-color_primaries', 'bt2020', '-color_trc', 'smpte2084',
                    '-colorspace', 'bt2020nc', '-color_range', 'tv', '-i', raw.name, '-frames:v', '2',
                    '-c:v', 'ffv1', fixture.name)
        stream, = probe(fixture)
        self.assertEqual((stream['color_transfer'], stream['color_primaries'], stream['color_space']),
                         ('smpte2084', 'bt2020', 'bt2020nc'))
        return fixture

    def test_pq_tone_map_produces_expected_sdr_pixels_and_tags(self):
        fixture = self.hdr10_fixture()
        output = self.work / 'sdr.mp4'
        self.ffmpeg('-i', fixture.name, '-vf', tone_map_filter('smpte2084'), '-c:v', 'libx264',
                    '-qp', '0', '-color_primaries', 'bt709', '-color_trc', 'bt709', '-colorspace', 'bt709',
                    '-color_range', 'tv', output.name)
        stream, = probe(output)
        self.assertEqual((stream['pix_fmt'], stream['color_transfer'], stream['color_primaries'],
                          stream['color_space'], stream['color_range']),
                         ('yuv420p', 'bt709', 'bt709', 'bt709', 'tv'))
        lumas, chromas = self.band_means(output)
        for nits, luma in zip(NITS, lumas):
            self.assertAlmostEqual(luma, expected_sdr_luma(nits), delta=2.5, msg=f'{nits} nits')
        for u, v in chromas:
            self.assertAlmostEqual(u, 128, delta=1.5)
            self.assertAlmostEqual(v, 128, delta=1.5)
        # The 1000-nit highlight reaches SDR white without clipping the 100-nit patch to it.
        self.assertGreater(lumas[3] - lumas[2], 80)

    def test_retagging_pq_as_bt709_is_not_tone_mapping(self):
        fixture = self.hdr10_fixture()
        retagged = self.work / 'retagged.mp4'
        self.ffmpeg('-i', fixture.name, '-vf', 'setparams=color_trc=bt709:color_primaries=bt709:colorspace=bt709,'
                    'format=yuv420p', '-c:v', 'libx264', '-qp', '0', retagged.name)
        lumas, _ = self.band_means(retagged)
        # Retagging keeps the PQ code values: the highlight stays grey and
        # diffuse white is dimmed, far from the tone-mapped expectations.
        self.assertLess(lumas[3], expected_sdr_luma(1000) - 40)
        self.assertLess(lumas[2], expected_sdr_luma(100) - 15)

    def test_hlg_route_reaches_sdr_white_and_stays_sdr_compatible(self):
        raw = self.work / 'hlg.yuv'
        # HLG code values from the BT.2100 OETF at fixed scene-light fractions.
        def hlg(e):
            a, b = 0.17883277, 1 - 4 * 0.17883277
            c = 0.5 - a * math.log(4 * a)
            signal = math.sqrt(3 * e) if e <= 1 / 12 else a * math.log(12 * e - b) + c
            return round(64 + 876 * signal)
        codes = [hlg(e) for e in (0, 0.01, 0.1, 1)]
        row = b''.join(code.to_bytes(2, 'little') * BAND for code in codes)
        chroma = (512).to_bytes(2, 'little') * (WIDTH // 2 * HEIGHT // 2)
        raw.write_bytes(row * HEIGHT + chroma * 2)
        output = self.work / 'hlg-sdr.mp4'
        self.ffmpeg('-f', 'rawvideo', '-pixel_format', 'yuv420p10le', '-video_size', f'{WIDTH}x{HEIGHT}',
                    '-framerate', '24', '-i', raw.name, '-frames:v', '2',
                    '-vf', tone_map_filter('arib-std-b67'), '-c:v', 'libx264', '-qp', '0', output.name)
        lumas, chromas = self.band_means(output)
        self.assertAlmostEqual(lumas[0], 16, delta=1.5)
        self.assertEqual(lumas, sorted(lumas))
        self.assertAlmostEqual(lumas[3], 235, delta=2.5)
        for u, v in chromas:
            self.assertAlmostEqual(u, 128, delta=1.5)
            self.assertAlmostEqual(v, 128, delta=1.5)
        # HLG is designed to be SDR-compatible: with a 1000-nit nominal peak the
        # mapped SDR luma stays close to the HLG signal itself (unlike PQ).
        signal = [16 + 219 * (code - 64) / 876 for code in codes]
        for luma, expected in zip(lumas, signal):
            self.assertAlmostEqual(luma, expected, delta=6)

    # libass drawings ({\p1}) render exact rectangles without depending on fonts.
    ASS_HEADER = ('[Script Info]\nScriptType: v4.00+\nPlayResX: 256\nPlayResY: 64\n\n'
                  '[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, '
                  'BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV\n'
                  'Style: Box,Helvetica,20,&H00FFFFFF,&H00000000,1,0,0,7,0,0,0\n\n'
                  '[Events]\nFormat: Layer, Start, End, Style, Text\n')

    def ass(self, name, x, start='0:00:01.00', end='0:00:02.00'):
        path = self.work / name
        path.write_text(self.ASS_HEADER + f'Dialogue: 0,{start},{end},Box,'
                        f'{{\\pos({x},16)\\p1}}m 0 0 l 32 0 32 32 0 32{{\\p0}}\n')
        return path

    def black_video(self, seconds=4):
        self.ffmpeg('-f', 'lavfi', '-i', f'color=black:size={WIDTH}x{HEIGHT}:rate=10:duration={seconds}',
                    '-c:v', 'ffv1', 'black.mkv')
        return 'black.mkv'

    def box_luma(self, media, seconds, x):
        y, _, _ = self.frame(media, seconds)
        return self.mean(y, WIDTH, x + 4, x + 28, 20, 44)

    def test_ass_burn_in_renders_only_during_its_event(self):
        source = self.black_video()
        self.ass('box.ass', 64)
        output = self.work / 'burned.mkv'
        self.ffmpeg('-i', source, '-vf', 'subtitles=box.ass', '-c:v', 'ffv1', output.name)
        self.assertLess(self.box_luma(output, 0.5, 64), 20)
        self.assertGreater(self.box_luma(output, 1.5, 64), 230)
        self.assertLess(self.box_luma(output, 1.5, 160), 20)
        self.assertLess(self.box_luma(output, 2.5, 64), 20)

    def test_subtitle_offset_shifts_burned_event(self):
        source = self.black_video()
        self.ass('box.ass', 64)
        output = self.work / 'offset.mkv'
        # A +1 s subtitle offset: render against shifted timestamps, then restore them.
        self.ffmpeg('-i', source, '-vf', 'setpts=PTS-1/TB,subtitles=box.ass,setpts=PTS+1/TB',
                    '-c:v', 'ffv1', output.name)
        self.assertLess(self.box_luma(output, 1.5, 64), 20)
        self.assertGreater(self.box_luma(output, 2.5, 64), 230)

    def test_embedded_track_selection_uses_exact_stream(self):
        source = self.black_video()
        self.ass('first.ass', 32)
        self.ass('second.ass', 160)
        self.ffmpeg('-i', source, '-i', 'first.ass', '-i', 'second.ass', '-map', '0', '-map', '1', '-map', '2',
                    '-c', 'copy', '-metadata:s:s:0', 'language=eng', '-metadata:s:s:1', 'language=eng',
                    'tracks.mkv')
        output = self.work / 'second.mkv'
        self.ffmpeg('-i', 'tracks.mkv', '-vf', 'subtitles=tracks.mkv:si=1', '-c:v', 'ffv1', output.name)
        self.assertGreater(self.box_luma(output, 1.5, 160), 230)
        self.assertLess(self.box_luma(output, 1.5, 32), 20)

    def test_text_renders_with_core_text_font_provider(self):
        source = self.black_video()
        path = self.work / 'text.ass'
        path.write_text(self.ASS_HEADER + 'Dialogue: 0,0:00:01.00,0:00:02.00,Box,{\\pos(16,16)}MOTION\n')
        output = self.work / 'text.mkv'
        self.ffmpeg('-i', source, '-vf', 'subtitles=text.ass', '-c:v', 'ffv1', output.name)
        y, _, _ = self.frame(output, 1.5)
        lit = sum(1 for value in y if value > 128)
        self.assertGreater(lit, 100, 'no glyphs rendered; libass found no system font')


if __name__ == '__main__':
    unittest.main()
