#!/usr/bin/env python3
"""Generate a frame-numbered H.264/AAC fixture; no source media is modified."""
import argparse,json,pathlib,subprocess,wave
import numpy as np
p=argparse.ArgumentParser();p.add_argument('directory',type=pathlib.Path);p.add_argument('--seconds',type=int,default=120);args=p.parse_args()
args.directory.mkdir(parents=True,exist_ok=True)
w,h,fps,rate=320,180,30,48000
# Twelve binary bars encode the frame index. Their centres survive YUV420/AAC recipes.
audio=args.directory/'signal.wav'
t=np.arange(args.seconds*rate,dtype=np.float64)/rate
samples=(np.sin(2*np.pi*(300*t+7*t*t))*.2*32767).astype('<i2')
with wave.open(str(audio),'wb') as f:f.setnchannels(1);f.setsampwidth(2);f.setframerate(rate);f.writeframes(samples.tobytes())
output=args.directory/'Frame-clock.mp4'
cmd=['ffmpeg','-v','error','-y','-f','rawvideo','-pixel_format','rgb24','-video_size',f'{w}x{h}','-framerate',str(fps),'-i','pipe:0','-i',str(audio),'-c:v','libx264','-preset','veryfast','-crf','18','-g','30','-bf','0','-pix_fmt','yuv420p','-c:a','aac','-b:a','160k','-movflags','+faststart',str(output)]
proc=subprocess.Popen(cmd,stdin=subprocess.PIPE)
for frame in range(args.seconds*fps):
 image=np.full((h,w,3),64,dtype=np.uint8)
 for bit in range(12):image[0:60,bit*24:(bit+1)*24,:]=224 if (frame>>bit)&1 else 16
 image[90:150,frame%(w-12):frame%(w-12)+12,:]=[220,100,40]
 proc.stdin.write(image.tobytes())
proc.stdin.close()
if proc.wait():raise SystemExit('ffmpeg failed')
(args.directory/'fixture.json').write_text(json.dumps({'width':w,'height':h,'fps':fps,'audio_rate':rate,'seconds':args.seconds,'frame_id_bits':12,'bar_width':24,'sample_y':30,'audio':'0.2*sin(2*pi*(300*t+7*t*t))'},indent=2)+'\n')
print(output)
