# Render benchmark: h264_nvenc vs libx264 (draft)

- Host: Model name:                              Intel(R) Core(TM) i5-10600K CPU @ 4.10GHz, 12 threads; GPUs: 0, NVIDIA GeForce RTX 3060 Ti, 580.178.04; 1, Tesla P40, 580.178.04
- ffmpeg version 6.1.1-3ubuntu5 Copyright (c) 2000-2023 the FFmpeg developers
- Source: testsrc2 1920x1080 30fps + seeded geq noise blend, raw yuv420p, 300 frames
- Runs per config: 3 (median reported). Encode-only timing.

| Config | Encoder | Median fps | fps range | Bitrate (kbps) | PSNR (dB) | SSIM |
|---|---|---|---|---|---|---|
| libx264_medium_crf23 | libx264 | 18.98 | 16.99–20.49 | 98032.3 | 33.922163 | 0.935791 |
| libx264_veryfast_crf23 | libx264 | 66.23 | 62.08–67.9 | 85107.3 | 32.996588 | 0.91271 |
| h264_nvenc_p4_cq28 | h264_nvenc | 245.46 | 240.2–245.47 | 11043.1 | 27.619973 | 0.569928 |
| h264_nvenc_p4_cq23 | h264_nvenc | 245.98 | 245.49–246.28 | 17315.9 | 27.85419 | 0.600034 |
| h264_nvenc_p4_cq18 | h264_nvenc | 244.31 | 243.79–247.43 | 17274.2 | 27.84794 | 0.599188 |
| h264_nvenc_p4_cq14 | h264_nvenc | 246.29 | 244.03–246.65 | 17351.1 | 27.847263 | 0.599355 |
| libx264_medium_crf18 | libx264 | 16.67 | 15.38–17.59 | 155166.5 | 39.280788 | 0.981271 |

## Commands

- `ffmpeg -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/src_1920x1080_30fps_300f.yuv -frames:v 300 -c:v libx264 -preset medium -crf 23 -pix_fmt yuv420p /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/libx264_medium_crf23.mp4`
- `ffmpeg -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/src_1920x1080_30fps_300f.yuv -frames:v 300 -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/libx264_veryfast_crf23.mp4`
- `ffmpeg -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/src_1920x1080_30fps_300f.yuv -frames:v 300 -c:v h264_nvenc -preset p4 -rc vbr -cq 28 -b:v 0 -gpu 0 -pix_fmt yuv420p /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/h264_nvenc_p4_cq28.mp4`
- `ffmpeg -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/src_1920x1080_30fps_300f.yuv -frames:v 300 -c:v h264_nvenc -preset p4 -rc vbr -cq 23 -b:v 0 -gpu 0 -pix_fmt yuv420p /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/h264_nvenc_p4_cq23.mp4`
- `ffmpeg -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/src_1920x1080_30fps_300f.yuv -frames:v 300 -c:v h264_nvenc -preset p4 -rc vbr -cq 18 -b:v 0 -gpu 0 -pix_fmt yuv420p /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/h264_nvenc_p4_cq18.mp4`
- `ffmpeg -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/src_1920x1080_30fps_300f.yuv -frames:v 300 -c:v h264_nvenc -preset p4 -rc vbr -cq 14 -b:v 0 -gpu 0 -pix_fmt yuv420p /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/h264_nvenc_p4_cq14.mp4`
- `ffmpeg -y -hide_banner -loglevel error -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/src_1920x1080_30fps_300f.yuv -frames:v 300 -c:v libx264 -preset medium -crf 18 -pix_fmt yuv420p /home/colton-mcclain/.hermes/profiles/agent-office/cache/scratch/cinvar-bench/libx264_medium_crf18.mp4`

## Caveats

- Single synthetic clip; real footage (motion, film grain) will change absolute numbers.
- Headline pair (x264 medium crf23 vs nvenc p4 cq28) is a speed comparison, not quality-matched.
- nvenc uses GPU 0 as reported by nvidia-smi (see environment.gpus); verify it is the RTX 3060 Ti.
