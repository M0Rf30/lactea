# Third-party notices

Lactea itself is licensed `MIT OR Apache-2.0` (see `LICENSE-MIT`, `LICENSE-APACHE`). It contains or
derives from the following third-party work.

## MilkDrop 2 (Nullsoft) — BSD-3-Clause

Applies to `third_party/md2/` (reference sources the renderer is ported from) and to
`crates/lactea-hlsl/src/include.fx`, which is copied verbatim from MilkDrop 2's `include.fx` and
shipped in the `lactea-hlsl` crate.

```
Copyright 2005-2013 Nullsoft, Inc.
All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

  * Redistributions of source code must retain the above copyright notice,
    this list of conditions and the following disclaimer.

  * Redistributions in binary form must reproduce the above copyright notice,
    this list of conditions and the following disclaimer in the documentation
    and/or other materials provided with the distribution.

  * Neither the name of Nullsoft nor the names of its contributors may be used to
    endorse or promote products derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## stb_image (Sean Barrett) — public domain / MIT

`lactea-jpeg` is a Rust port of the JPEG decoding arithmetic of `stb_image.h` v2.30 (integer IDCT,
chroma resampling, YCbCr conversion) so that textures decode byte for byte as in projectM. stb_image
is dual-licensed public domain / MIT (Sean Barrett); no stb source is shipped.

## projectM and projectm-eval — external test oracles only

projectM (LGPL-2.1) and projectm-eval (MIT) are built locally by the maintainers' test tooling
(`oracle/`, `crates/lactea-eel-oracle`) to produce reference output. They are neither linked into nor
distributed with any Lactea crate or release artifact; their sources are git-ignored and never
copied.
