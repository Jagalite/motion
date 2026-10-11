# Motion paced HLS candidate

This derivative is under qualification; it is not the release provider pin.
`motion-paced.patch` applies to upstream Shaka 5.2.11. The default remains off.
Motion's candidate deployment enables `manifest.hls.motionPaced` only for its
explicit live HLS sources. Do not enable this for ordinary clock-driven broadcasts.

The core publishes retained segment facts and authorizes generation tickets. This
provider adapter maps those manifest observations to a seekable interval, taking
the intersection of retained audio/video indexes. Encoder pauses must neither
expire retained media nor invent future segments. Empty indexes clear the range.
No server authorization, generation ownership, or retention policy moves here.

Apply with `patch -p1 < motion-paced.patch` in the verified source tree. Run:

```
MOTION_SHAKA_SOURCE=/path/to/patched/source node --test scripts/test_paced_shaka.mjs
```

The tests load upstream production timeline/parser/index classes. They cover
clock pauses, rolling intersection, empty-index eviction/recovery, zero bounds,
finite completion transition, and unchanged default policy. They do not yet prove
actual ENDLIST parsing, deployment startup, or installed-product acceptance.

Compile through the shared build scheduler, with Java and npm on PATH:

```
python3 build/build.py --name motion --mode release --skip-ts --skip-worker +@complete -@ui -@cast -@ads
```

The output is `dist/shaka-player.motion.js`. A derivative deployment must update
its complete asset closure identity and runtime identity inventory, preserve
source/patch/build provenance, and pass the product acceptance matrix. It must
not claim the upstream provider's unchanged identity or qualification.
