// refrender: deterministic, instrumented projectM reference renderer for lactea.
// See oracle/README.md. Linked against the patched libprojectM-4 built by oracle/build.sh.
#define _GNU_SOURCE
#include <EGL/egl.h>
#include <EGL/eglext.h>
#define GL_GLEXT_PROTOTYPES
#include <GL/gl.h>
#include <GL/glext.h>
#include <errno.h>
#include <math.h>
#include <png.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include <projectM-4/projectM.h>

#ifndef PROJECTM_SHA
#define PROJECTM_SHA "unknown"
#endif

// ---- exported by the oracle patch (FdOracle.cpp) ------------------------------------------------
typedef void (*fd_image_sink)(const char *name, int frame, int w, int h, const uint8_t *rgba, void *user);
extern void fd_oracle_set_image_sink(fd_image_sink sink, void *user);
extern const char *fd_oracle_info_json(void);
extern int fd_oracle_shader_error_count(void);
extern void fd_oracle_reset_errors(void);

// ---- options ------------------------------------------------------------------------------------
static struct {
    const char *preset, *audio, *out, *stages, *tex_dir;
    int w, h, mx, my, fps, frames;
    uint64_t seed;
    float tox, toy;
    bool vars, textures, default_fb;
    const char *preset2, *transition_static;
    int switch_frame, transition_shader;
    double soft_cut;
    bool start_clean;
    const char *touch;
} opt = {NULL, NULL, NULL, "final,main,warped,blur1,blur2,blur3", NULL, 640, 360, 48, 36, 60, 300, 1, 0.f, 0.f, false, false, false,
         NULL, NULL, -1, -1, 0.0, false, NULL};

static const char *kStages[6] = {"final", "main", "warped", "blur1", "blur2", "blur3"};

static void die(const char *msg) { fprintf(stderr, "refrender: %s\n", msg); exit(1); }

static void usage(void) {
    fprintf(stderr,
            "usage: refrender --preset P --audio WAV --out DIR [--size WxH] [--mesh WxH] [--fps N] [--frames N]\n"
            "                 [--seed N] [--texel-offset X,Y] [--stages final,main,warped,blur1,blur2,blur3[,warpsrc]|all|none]\n"
            "                 [--vars] [--textures] [--texture-dir DIR]\n"
            "                 [--touch 'N:touch,x,y,pressure,type;N:drag,x,y,pressure;N:destroy,x,y;N:destroy_all;...']\n"
            "                 [--preset2 P --switch-frame N --soft-cut S [--start-clean 0|1]\n"
            "                  [--transition-shader 0..5] [--transition-static a,b,c,d]]\n"
            "                 (--stages may also contain old_out,new_out with --preset2; `all` adds them)\n"
            "exit: 0 ok, 1 usage/IO/GL error, 2 preset load failure, 3 shader compile/link/translate errors\n");
    exit(1);
}

static bool stage_on(const char *s);
static void parse_args(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        const char *a = argv[i];
#define NEXT() (i + 1 < argc ? argv[++i] : (usage(), (char *)NULL))
        if (!strcmp(a, "--preset")) opt.preset = NEXT();
        else if (!strcmp(a, "--audio")) opt.audio = NEXT();
        else if (!strcmp(a, "--out")) opt.out = NEXT();
        else if (!strcmp(a, "--size")) { if (sscanf(NEXT(), "%dx%d", &opt.w, &opt.h) != 2) usage(); }
        else if (!strcmp(a, "--mesh")) { if (sscanf(NEXT(), "%dx%d", &opt.mx, &opt.my) != 2) usage(); }
        else if (!strcmp(a, "--fps")) opt.fps = atoi(NEXT());
        else if (!strcmp(a, "--frames")) opt.frames = atoi(NEXT());
        else if (!strcmp(a, "--seed")) opt.seed = strtoull(NEXT(), NULL, 10);
        else if (!strcmp(a, "--texel-offset")) { if (sscanf(NEXT(), "%f,%f", &opt.tox, &opt.toy) != 2) usage(); }
        else if (!strcmp(a, "--stages")) opt.stages = NEXT();
        else if (!strcmp(a, "--vars")) opt.vars = true;
        else if (!strcmp(a, "--textures")) opt.textures = true;
        else if (!strcmp(a, "--default-fb")) opt.default_fb = true;  // debug: render to the pbuffer instead of an FBO
        else if (!strcmp(a, "--texture-dir")) opt.tex_dir = NEXT();
        else if (!strcmp(a, "--preset2")) opt.preset2 = NEXT();
        else if (!strcmp(a, "--switch-frame")) opt.switch_frame = atoi(NEXT());
        else if (!strcmp(a, "--soft-cut")) opt.soft_cut = atof(NEXT());
        else if (!strcmp(a, "--start-clean")) opt.start_clean = atoi(NEXT()) != 0;
        else if (!strcmp(a, "--transition-shader")) opt.transition_shader = atoi(NEXT());
        else if (!strcmp(a, "--transition-static")) opt.transition_static = NEXT();
        else if (!strcmp(a, "--touch")) opt.touch = NEXT();
        else usage();
    }
    if (!opt.preset || !opt.audio || !opt.out) usage();
    if (opt.w <= 0 || opt.h <= 0 || opt.fps <= 0 || opt.frames < 0) usage();
    if (!strcmp(opt.stages, "none")) opt.stages = "";
    if (opt.preset2 && opt.switch_frame < 0) die("--preset2 requires --switch-frame N");
    if (!opt.preset2 && (opt.switch_frame >= 0 || opt.soft_cut != 0.0 || opt.start_clean || opt.transition_shader >= 0 || opt.transition_static))
        die("--switch-frame/--soft-cut/--start-clean/--transition-* require --preset2");
    if (opt.transition_shader >= 6) die("--transition-shader must be 0..5");
    if (opt.transition_static) {
        int a[4];
        if (sscanf(opt.transition_static, "%d,%d,%d,%d", a, a + 1, a + 2, a + 3) != 4) die("--transition-static expects a,b,c,d (int32)");
    }
    if (stage_on("all")) {  // "all" token expands to the six ABI stages (extras like warpsrc may follow)
        static char expanded[256];
        snprintf(expanded, sizeof expanded, "final,main,warped,blur1,blur2,blur3%s%s", stage_on("warpsrc") ? ",warpsrc" : "",
                 opt.preset2 ? ",old_out,new_out" : "");
        opt.stages = expanded;
    }
}

static bool stage_on(const char *s) {
    size_t n = strlen(s);
    for (const char *p = opt.stages; *p;) {
        const char *e = strchr(p, ',');
        size_t l = e ? (size_t)(e - p) : strlen(p);
        if (l == n && !strncmp(p, s, n)) return true;
        if (!e) break;
        p = e + 1;
    }
    return false;
}

// ---- fs helpers -----------------------------------------------------------------------------------
static void mkdirs(const char *path) {
    char buf[4096];
    snprintf(buf, sizeof buf, "%s", path);
    for (char *p = buf + 1; *p; p++)
        if (*p == '/') { *p = 0; mkdir(buf, 0777); *p = '/'; }
    mkdir(buf, 0777);
}

// ---- PNG sink -------------------------------------------------------------------------------------
static void write_png(const char *path, int w, int h, const uint8_t *rgba) {
    FILE *f = fopen(path, "wb");
    if (!f) { fprintf(stderr, "refrender: cannot write %s: %s\n", path, strerror(errno)); exit(1); }
    png_structp png = png_create_write_struct(PNG_LIBPNG_VER_STRING, NULL, NULL, NULL);
    png_infop info = png_create_info_struct(png);
    if (setjmp(png_jmpbuf(png))) { fprintf(stderr, "refrender: png error %s\n", path); exit(1); }
    png_init_io(png, f);
    png_set_IHDR(png, info, w, h, 8, PNG_COLOR_TYPE_RGBA, PNG_INTERLACE_NONE, PNG_COMPRESSION_TYPE_DEFAULT, PNG_FILTER_TYPE_DEFAULT);
    png_set_compression_level(png, 4);
    png_write_info(png, info);
    for (int y = 0; y < h; y++) png_write_row(png, rgba + (size_t)y * w * 4);
    png_write_end(png, info);
    png_destroy_write_struct(&png, &info);
    fclose(f);
}

static void sink(const char *name, int frame, int w, int h, const uint8_t *rgba, void *user) {
    (void)user;
    char dir[4096], path[4400];
    if (frame < 0) {  // static texture: name = "textures/<n>"
        snprintf(dir, sizeof dir, "%s/textures", opt.out);
        mkdirs(dir);
        snprintf(path, sizeof path, "%s/%s.png", opt.out, name);
    } else {
        snprintf(dir, sizeof dir, "%s/%s", opt.out, name);
        mkdirs(dir);
        snprintf(path, sizeof path, "%s/%05d.png", dir, frame);
    }
    write_png(path, w, h, rgba);
}

// ---- WAV --------------------------------------------------------------------------------------------
typedef struct { float *data; uint64_t frames; uint32_t rate; } Audio;  // interleaved stereo float

static uint32_t rd32(const uint8_t *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }
static uint16_t rd16(const uint8_t *p) { return p[0] | p[1] << 8; }

static Audio load_wav(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) { fprintf(stderr, "refrender: cannot open audio %s\n", path); exit(1); }
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc(sz);
    if (fread(buf, 1, sz, f) != (size_t)sz) die("audio read failed");
    fclose(f);
    if (sz < 12 || memcmp(buf, "RIFF", 4) || memcmp(buf + 8, "WAVE", 4)) die("not a RIFF/WAVE file");
    int fmt = 0, ch = 0, bits = 0;
    uint32_t rate = 0;
    const uint8_t *data = NULL;
    uint32_t dlen = 0;
    for (long p = 12; p + 8 <= sz;) {
        uint32_t cl = rd32(buf + p + 4);
        const uint8_t *c = buf + p + 8;
        if (!memcmp(buf + p, "fmt ", 4)) {
            fmt = rd16(c); ch = rd16(c + 2); rate = rd32(c + 4); bits = rd16(c + 14);
            if (fmt == 0xFFFE && cl >= 26) fmt = rd16(c + 24);  // WAVE_FORMAT_EXTENSIBLE subformat
        } else if (!memcmp(buf + p, "data", 4)) {
            data = c;
            dlen = (long)(p + 8 + cl) > sz ? (uint32_t)(sz - p - 8) : cl;
            break;
        }
        p += 8 + cl + (cl & 1);
    }
    if (!data || ch < 1) die("WAV: missing fmt/data");
    if (!((fmt == 3 && bits == 32) || (fmt == 1 && bits == 16))) die("WAV: only float32 and int16 PCM supported");
    uint64_t n = dlen / (ch * bits / 8);
    Audio a = {malloc(n * 2 * sizeof(float)), n, rate};
    for (uint64_t i = 0; i < n; i++) {
        float s[2];
        for (int c = 0; c < 2; c++) {
            int cc = c < ch ? c : 0;  // mono -> both channels
            const uint8_t *q = data + (i * ch + cc) * (bits / 8);
            if (fmt == 3) { uint32_t u = rd32(q); memcpy(&s[c], &u, 4); }
            else s[c] = (int16_t)rd16(q) / 32768.0f;
        }
        a.data[i * 2] = s[0];
        a.data[i * 2 + 1] = s[1];
    }
    free(buf);
    return a;
}

// ---- preset failure callback ---------------------------------------------------------------------------
static bool g_preset_failed = false;
static char g_preset_error[1024];
static void on_preset_failed(const char *file, const char *msg, void *user) {
    (void)user;
    g_preset_failed = true;
    snprintf(g_preset_error, sizeof g_preset_error, "%s", msg ? msg : "");
    fprintf(stderr, "refrender: PRESET LOAD FAILED: %s: %s\n", file ? file : "?", msg ? msg : "");
}

static void json_str(FILE *f, const char *s) {
    fputc('"', f);
    for (; *s; s++) {
        if (*s == '"' || *s == '\\') { fputc('\\', f); fputc(*s, f); }
        else if (*s == '\n') fputs("\\n", f);
        else if ((unsigned char)*s < 0x20) fprintf(f, "\\u%04x", *s);
        else fputc(*s, f);
    }
    fputc('"', f);
}

// GL_RENDERER of the context that produced the reference (provenance: line/point rasterization and
// float rounding are driver-specific, so references from different drivers are not interchangeable).
static char g_gl_renderer[256] = "unknown";
static int g_preset2_loaded = 0;  // set to 1 once --preset2 loaded (only used when --preset2 is given)

static void write_oracle_json(bool loaded, int frames_done) {
    char path[4400];
    snprintf(path, sizeof path, "%s/oracle.json", opt.out);
    FILE *f = fopen(path, "wb");
    if (!f) return;
    const char *info = fd_oracle_info_json();  // {...}
    size_t n = strlen(info);
    fwrite(info, 1, n - 1, f);  // without closing brace
    fprintf(f, ", \"preset_loaded\": %s, \"preset_error\": ", loaded ? "true" : "false");
    json_str(f, g_preset_error);
    fprintf(f, ", \"frames_rendered\": %d, \"projectm\": \"%s\", \"gl_renderer\": ", frames_done, PROJECTM_SHA);
    json_str(f, g_gl_renderer);
    if (opt.preset2) {  // only with --preset2, so single-preset oracle.json stays byte-identical
        fprintf(f, ", \"preset2\": ");
        json_str(f, opt.preset2);
        fprintf(f, ", \"preset2_loaded\": %s, \"switch_frame\": %d, \"soft_cut\": %.17g, \"start_clean\": %s",
                g_preset2_loaded ? "true" : "false", opt.switch_frame, opt.soft_cut, opt.start_clean ? "true" : "false");
    }
    fputs("}\n", f);
    fclose(f);
}

static void write_meta(void) {
    char path[4400];
    snprintf(path, sizeof path, "%s/meta.json", opt.out);
    FILE *f = fopen(path, "wb");
    if (!f) die("cannot write meta.json");
    fprintf(f, "{\n  \"producer\": \"projectm@%.12s\",\n  \"preset\": ", PROJECTM_SHA);
    json_str(f, opt.preset);
    fprintf(f, ",\n  \"audio\": ");
    json_str(f, opt.audio);
    fprintf(f,
            ",\n  \"config\": {\"width\": %d, \"height\": %d, \"mesh\": [%d, %d], \"fps\": %d, \"frames\": %d, \"seed\": %llu, "
            "\"texel_offset\": [%.9g, %.9g]},\n  \"stages\": [",
            opt.w, opt.h, opt.mx, opt.my, opt.fps, opt.frames, (unsigned long long)opt.seed, opt.tox, opt.toy);
    bool first = true;
    for (int i = 0; i < 6; i++)
        if (stage_on(kStages[i])) { fprintf(f, "%s\"%s\"", first ? "" : ", ", kStages[i]); first = false; }
    fprintf(f, "],\n  \"has_vars\": %s\n}\n", opt.vars ? "true" : "false");
    fclose(f);
}

// projectm_touch*: upstream stubs (ProjectM.cpp "UNIMPLEMENTED"); the harness only proves they change nothing.
static void apply_touches(projectm_handle pm, int frame) {
    if (!opt.touch) return;
    char *spec = strdup(opt.touch), *save = NULL;
    for (char *it = strtok_r(spec, ";", &save); it; it = strtok_r(NULL, ";", &save)) {
        int n;
        char cmd[32];
        float x = 0, y = 0;
        int p = 0, t = 0;
        if (sscanf(it, "%d:%31[a-z_]", &n, cmd) != 2) die("bad --touch item");
        if (n != frame) continue;
        const char *args = strchr(it, ',');
        if (!strcmp(cmd, "touch")) {
            if (!args || sscanf(args, ",%f,%f,%d,%d", &x, &y, &p, &t) != 4) die("bad --touch item");
            projectm_touch(pm, x, y, p, (projectm_touch_type)t);
        } else if (!strcmp(cmd, "drag")) {
            if (!args || sscanf(args, ",%f,%f,%d", &x, &y, &p) != 3) die("bad --touch item");
            projectm_touch_drag(pm, x, y, p);
        } else if (!strcmp(cmd, "destroy")) {
            if (!args || sscanf(args, ",%f,%f", &x, &y) != 2) die("bad --touch item");
            projectm_touch_destroy(pm, x, y);
        } else if (!strcmp(cmd, "destroy_all")) {
            projectm_touch_destroy_all(pm);
        } else die("unknown --touch command");
    }
    free(spec);
}

// ---- main -------------------------------------------------------------------------------------------------
int main(int argc, char **argv) {
    parse_args(argc, argv);
    mkdirs(opt.out);

    // Instrumentation config consumed by the patched libprojectM.
    char seedbuf[32];
    snprintf(seedbuf, sizeof seedbuf, "%llu", (unsigned long long)opt.seed);
    setenv("FD_SEED", seedbuf, 1);
    setenv("FD_DUMP_DIR", opt.out, 1);
    setenv("FD_DUMP_STAGES", opt.stages, 1);
    setenv("FD_DUMP_VARS", opt.vars ? "1" : "0", 1);
    setenv("FD_DUMP_TEXTURES", opt.textures ? "1" : "0", 1);
    fd_oracle_set_image_sink(sink, NULL);

    Audio audio = load_wav(opt.audio);

    EGLDisplay d = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (d == EGL_NO_DISPLAY || !eglInitialize(d, 0, 0)) {
        // No WAYLAND_DISPLAY/DISPLAY (CI, ssh, services): Mesa's surfaceless platform still uses the GPU.
        PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
            (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
        d = get_platform_display ? get_platform_display(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL)
                                 : EGL_NO_DISPLAY;
        if (d == EGL_NO_DISPLAY || !eglInitialize(d, 0, 0)) die("EGL init failed (default and surfaceless platforms)");
    }
    EGLint ca[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT, EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
                   EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE};
    EGLConfig cfg;
    EGLint ncfg;
    if (!eglChooseConfig(d, ca, &cfg, 1, &ncfg) || !ncfg) die("no EGL config");
    EGLint pa[] = {EGL_WIDTH, opt.w, EGL_HEIGHT, opt.h, EGL_NONE};
    EGLSurface s = eglCreatePbufferSurface(d, cfg, pa);
    eglBindAPI(EGL_OPENGL_API);
    EGLint xa[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 3, EGL_CONTEXT_OPENGL_PROFILE_MASK,
                   EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
    EGLContext ctx = eglCreateContext(d, cfg, EGL_NO_CONTEXT, xa);
    if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(d, s, s, ctx)) die("GL 3.3 core context failed");
    snprintf(g_gl_renderer, sizeof g_gl_renderer, "%s", (const char *)glGetString(GL_RENDERER));

    // Target FBO (RGBA8) the final image is rendered into.
    GLuint tex, fbo;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, opt.w, opt.h, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) die("target FBO incomplete");
    glViewport(0, 0, opt.w, opt.h);
    glClearColor(0, 0, 0, 0);
    glClear(GL_COLOR_BUFFER_BIT);

    projectm_handle pm = projectm_create();
    if (!pm) die("projectm_create failed");
    fd_oracle_reset_errors();  // ignore anything the idle preset did
    projectm_set_preset_switch_failed_event_callback(pm, on_preset_failed, NULL);
    projectm_set_window_size(pm, opt.w, opt.h);
    projectm_set_mesh_size(pm, opt.mx, opt.my);
    projectm_set_fps(pm, opt.fps);
    projectm_set_texel_offset(pm, opt.tox, opt.toy);
    projectm_set_preset_locked(pm, true);
    projectm_set_hard_cut_enabled(pm, false);
    projectm_set_soft_cut_duration(pm, 0.0);
    projectm_set_preset_duration(pm, 1e9);
    projectm_set_preset_start_clean(pm, true);
    if (opt.tex_dir) {
        const char *paths[1] = {opt.tex_dir};
        projectm_set_texture_search_paths(pm, paths, 1);
    }
    projectm_set_frame_time(pm, 0.0);

    projectm_load_preset_file(pm, opt.preset, false);
    if (g_preset_failed) {
        write_oracle_json(false, 0);
        fprintf(stderr, "refrender: aborting (preset did not load)\n");
        return 2;
    }

    write_meta();
    if (opt.preset2 && opt.transition_shader >= 0) {  // honoured by the patched libprojectM only because FD_SEED is set
        char b[16];
        snprintf(b, sizeof b, "%d", opt.transition_shader);
        setenv("FD_TRANSITION_SHADER", b, 1);
    }
    if (opt.preset2 && opt.transition_static) setenv("FD_TRANSITION_STATIC", opt.transition_static, 1);

    const uint64_t SR = audio.rate;
    uint8_t *px = malloc((size_t)opt.w * opt.h * 4), *flip = malloc((size_t)opt.w * opt.h * 4);
    float *zero = calloc(4096 * 2, sizeof(float));
    bool want_final = stage_on("final");
    int done = 0;
    for (int n = 0; n < opt.frames; n++) {
        apply_touches(pm, n);
        if (opt.preset2 && n == opt.switch_frame) {  // soft cut to the second preset (see README "Transitions")
            projectm_set_soft_cut_duration(pm, opt.soft_cut);
            projectm_set_preset_start_clean(pm, opt.start_clean);
            projectm_load_preset_file(pm, opt.preset2, true);
            if (g_preset_failed) {
                g_preset2_loaded = 0;
                write_oracle_json(true, done);
                fprintf(stderr, "refrender: aborting (preset2 did not load)\n");
                return 2;
            }
            g_preset2_loaded = 1;
        }
        uint64_t s0 = (uint64_t)n * SR / opt.fps, e0 = (uint64_t)(n + 1) * SR / opt.fps;
        uint64_t have = s0 < audio.frames ? (e0 < audio.frames ? e0 : audio.frames) - s0 : 0;
        if (have) projectm_pcm_add_float(pm, audio.data + s0 * 2, (unsigned)have, PROJECTM_STEREO);
        if (have < e0 - s0) {  // audio exhausted: pad with silence so frame/pcm accounting stays exact
            uint64_t miss = e0 - s0 - have;
            while (miss) {
                uint64_t c = miss > 4096 ? 4096 : miss;
                projectm_pcm_add_float(pm, zero, (unsigned)c, PROJECTM_STEREO);
                miss -= c;
            }
            if (n == 0 || s0 >= audio.frames) { static bool warned; if (!warned) { fprintf(stderr, "refrender: warning: audio shorter than %d frames; padding with silence\n", opt.frames); warned = true; } }
        }
        projectm_set_frame_time(pm, (double)n / opt.fps);
        GLuint target = opt.default_fb ? 0 : fbo;
        glBindFramebuffer(GL_FRAMEBUFFER, target);
        glViewport(0, 0, opt.w, opt.h);
        glClearColor(0, 0, 0, 0);
        glClear(GL_COLOR_BUFFER_BIT);
        projectm_opengl_render_frame_fbo(pm, target);

        if (want_final) {
            glBindFramebuffer(GL_READ_FRAMEBUFFER, target);
            glPixelStorei(GL_PACK_ALIGNMENT, 1);
            glReadPixels(0, 0, opt.w, opt.h, GL_RGBA, GL_UNSIGNED_BYTE, px);
            size_t stride = (size_t)opt.w * 4;
            for (int y = 0; y < opt.h; y++) memcpy(flip + (size_t)y * stride, px + (size_t)(opt.h - 1 - y) * stride, stride);
            sink("final", n, opt.w, opt.h, flip, NULL);
        }
        done++;
    }

    int errs = fd_oracle_shader_error_count();
    write_oracle_json(true, done);
    projectm_destroy(pm);
    if (errs) {
        fprintf(stderr, "refrender: SHADER ERRORS: %d (see oracle.json; projectM fell back to default shaders)\n", errs);
        return 3;
    }
    return 0;
}

