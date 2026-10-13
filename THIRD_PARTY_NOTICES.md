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

## BeatDrop Music Visualizer (Maxim Volskiy and contributors) — BSD-3-Clause

`crates/lactea-hlsl/src/wrap.rs` (the BeatDrop shader prelude, from BeatDrop's `include.fx`) and
`crates/lactea-audio/src/beatdrop.rs` (the shader FFT, from BeatDrop's `plugin.cpp`) are ports of that
project's code (https://github.com/OfficialIncubo/BeatDrop-Music-Visualizer, branch `master`).

```
BSD 3-Clause License

Copyright (c) 2018 Maxim Volskiy and individual contributors
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

  * Redistributions of source code must retain the above copyright notice,
    this list of conditions and the following disclaimer.

  * Redistributions in binary form must reproduce the above copyright notice,
    this list of conditions and the following disclaimer in the documentation
    and/or other materials provided with the distribution.

  * Neither the name of the copyright holder nor the names of its
    contributors may be used to endorse or promote products derived from
    this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
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

## ns-eel2 (Cockos / Nullsoft) — zlib-style licence

`crates/lactea-eel/src/ns_pre.rs` is a Rust port of `preprocessCode` from `nseel-compiler.c`, and the
`Dialect::NsEel2` paths of `crates/lactea-eel/src/lexer.rs` and `parser.rs` reproduce the behaviour of
`nseel-lextab.c` (scanner) and `nseel-caltab.c` (grammar). These are altered versions, not the original
source; no ns-eel2 source is shipped. Original notice (identical in the ns-eel2 sources):

```
Expression Evaluator Library (NS-EEL) v2
Copyright (C) 2004-2008 Cockos Incorporated
Copyright (C) 1999-2003 Nullsoft, Inc.

This software is provided 'as-is', without any express or implied
warranty.  In no event will the authors be held liable for any damages
arising from the use of this software.

Permission is granted to anyone to use this software for any purpose,
including commercial applications, and to alter it and redistribute it
freely, subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not
   claim that you wrote the original software. If you use this software
   in a product, an acknowledgment in the product documentation would be
   appreciated but is not required.
2. Altered source versions must be plainly marked as such, and must not be
   misrepresented as being the original software.
3. This notice may not be removed or altered from any source distribution.
```

## projectM and projectm-eval — external test oracles only

projectM (LGPL-2.1) and projectm-eval (MIT) are built locally by the maintainers' test tooling
(`oracle/`, `crates/lactea-eel-oracle`) to produce reference output. They are neither linked into nor
distributed with any Lactea crate or release artifact; their sources are git-ignored and never
copied.
