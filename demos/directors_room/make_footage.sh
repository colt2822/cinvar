#!/usr/bin/env bash
# Procedural footage with real motion (no third-party media, nothing to license).
# Usage: make_footage.sh <outdir>
set -euo pipefail
out="${1:-demos/directors_room/footage}"
mkdir -p "$out"
ff() { ffmpeg -hide_banner -loglevel error -y "$@"; }
V="-c:v libx264 -preset veryfast -crf 18 -pix_fmt yuv420p"

# 1080p landscape shots, each with different motion and palette
ff -f lavfi -i "mandelbrot=s=1920x1080:r=30:end_scale=0.01:start_scale=3:inner=convergence" -t 9 $V "$out/fractal-dive.mp4"
ff -f lavfi -i "gradients=s=1920x1080:r=30:d=9:speed=0.04:nb_colors=4:c0=0x0b1d51:c1=0xff5e3a:c2=0x1b998b:c3=0xf4d35e" $V "$out/aurora.mp4"
ff -f lavfi -i "gradients=s=1920x1080:r=30:d=8:speed=0.09:nb_colors=3:c0=0x111111:c1=0xe63946:c2=0xf1faee" $V "$out/ember-flow.mp4"
ff -f lavfi -i "testsrc2=s=1920x1080:r=30:d=7" $V "$out/signal-grid.mp4"
# a bright "product" card: coloured disc on white, gentle drift (for the ad challenge)
ff -f lavfi -i "color=c=white:s=1920x1080:r=30:d=6,geq=lum='if(lt(hypot(X-960-120*sin(T*1.2),Y-540-30*cos(T*1.2)),260),60,235)':cb=128:cr='if(lt(hypot(X-960-120*sin(T*1.2),Y-540-30*cos(T*1.2)),260),200,128)'" $V "$out/product-disc.mp4"

# music: a pulsing minor chord bed (kick + pad), 40 s stereo
ff -f lavfi -i "aevalsrc='0.35*sin(2*PI*55*t)*exp(-9*mod(t,0.5))+0.10*sin(2*PI*110*t)+0.08*sin(2*PI*130.81*t)+0.07*sin(2*PI*164.81*t)+0.05*sin(2*PI*220*t)*(0.6+0.4*sin(2*PI*0.25*t))|0.35*sin(2*PI*55*t)*exp(-9*mod(t,0.5))+0.10*sin(2*PI*110*t)+0.08*sin(2*PI*130.81*t)+0.07*sin(2*PI*164.81*t)+0.05*sin(2*PI*220*t)*(0.6+0.4*sin(2*PI*0.25*t))':s=48000:d=40" -ac 2 "$out/music-bed.wav"
# voice stand-in: syllable-like bursts of a voiced tone (NOT real speech; used to exercise ducking/captions timing)
ff -f lavfi -i "aevalsrc='0.45*(sin(2*PI*140*t)+0.5*sin(2*PI*280*t)+0.25*sin(2*PI*420*t))*max(0,sin(2*PI*3.6*t))*(0.6+0.4*sin(2*PI*0.7*t))|0.45*(sin(2*PI*140*t)+0.5*sin(2*PI*280*t)+0.25*sin(2*PI*420*t))*max(0,sin(2*PI*3.6*t))*(0.6+0.4*sin(2*PI*0.7*t))':s=48000:d=12" -ac 2 "$out/voiceover-standin.wav"
ls -la "$out"
