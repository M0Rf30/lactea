// Probe: where does the GPU's clip -> window -> 1/256-px snap put a vertex?  (Mesa iris, as the oracle.)
// usage: snap W H axis mode in.f32 out.i32
//   axis 0: test the x coordinate, axis 1: the y coordinate (GL window y, up).
//   mode 0: GL_POINTS (pixel = floor of the snapped position); mode 1: a GL_LINES segment that is exactly
//           vertical (axis 0) / horizontal (axis 1) at the tested coordinate, GL_LINE_SMOOTH on (diamond-exit);
//           mode 2: same segment with GL_LINE_SMOOTH off.
//   in.f32: N clip-space values (f32) of the tested coordinate; out.i32: N lit pixel indices along the axis (-1: none,
//           -2: more than one).
// The other coordinate of case i of a batch is the centre of pixel (i % other_size) so a whole batch (one draw) is
// read back with one glReadPixels: a case lights a pixel in its own row/column only.
#define _GNU_SOURCE
#include <EGL/egl.h>
#include <EGL/eglext.h>
#define GL_GLEXT_PROTOTYPES
#include <GL/gl.h>
#include <GL/glext.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void die(const char *m) { fprintf(stderr, "%s\n", m); exit(1); }

static GLuint sh(GLenum t, const char *s) {
    GLuint o = glCreateShader(t);
    glShaderSource(o, 1, &s, NULL);
    glCompileShader(o);
    GLint ok;
    glGetShaderiv(o, GL_COMPILE_STATUS, &ok);
    if (!ok) { char b[2048]; glGetShaderInfoLog(o, 2048, NULL, b); fprintf(stderr, "%s\n", b); exit(1); }
    return o;
}

int main(int argc, char **argv) {
    if (argc < 7) die("usage: snap W H axis mode in.f32 out.i32");
    int W = atoi(argv[1]), H = atoi(argv[2]), axis = atoi(argv[3]), mode = atoi(argv[4]);
    EGLDisplay d = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (d == EGL_NO_DISPLAY || !eglInitialize(d, 0, 0)) {
        PFNEGLGETPLATFORMDISPLAYEXTPROC gp = (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
        d = gp ? gp(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL) : EGL_NO_DISPLAY;
        if (d == EGL_NO_DISPLAY || !eglInitialize(d, 0, 0)) die("EGL init failed");
    }
    EGLint ca[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT, EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
                   EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE};
    EGLConfig cfg; EGLint ncfg;
    if (!eglChooseConfig(d, ca, &cfg, 1, &ncfg) || !ncfg) die("no EGL config");
    EGLint pa[] = {EGL_WIDTH, W, EGL_HEIGHT, H, EGL_NONE};
    EGLSurface s = eglCreatePbufferSurface(d, cfg, pa);
    eglBindAPI(EGL_OPENGL_API);
    EGLint xa[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 3, EGL_CONTEXT_OPENGL_PROFILE_MASK,
                   EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
    EGLContext ctx = eglCreateContext(d, cfg, EGL_NO_CONTEXT, xa);
    if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(d, s, s, ctx)) die("GL ctx");

    FILE *f = fopen(argv[5], "rb");
    if (!f) die("in open");
    fseek(f, 0, SEEK_END);
    long nb = ftell(f);
    fseek(f, 0, SEEK_SET);
    int n = (int)(nb / 4);
    float *in = malloc(nb);
    if (fread(in, 4, n, f) != (size_t)n) die("in read");
    fclose(f);

    const char *vs = "#version 330 core\nlayout(location=0) in vec2 pos;\nvoid main(){ gl_Position = vec4(pos, 0.0, 1.0); }\n";
    const char *fs = "#version 330 core\nlayout(location=0) out vec4 o;\nvoid main(){ o = vec4(1.0); }\n";
    GLuint p = glCreateProgram();
    glAttachShader(p, sh(GL_VERTEX_SHADER, vs)); glAttachShader(p, sh(GL_FRAGMENT_SHADER, fs));
    glLinkProgram(p); glUseProgram(p);
    GLuint vao, vbo, rt, fbo;
    glGenVertexArrays(1, &vao); glBindVertexArray(vao);
    glGenBuffers(1, &vbo); glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glEnableVertexAttribArray(0); glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 8, (void *)0);
    glGenTextures(1, &rt); glBindTexture(GL_TEXTURE_2D, rt);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glGenFramebuffers(1, &fbo); glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) die("fbo incomplete");
    glViewport(0, 0, W, H);
    glDisable(GL_BLEND); glDisable(GL_DEPTH_TEST); glDisable(GL_CULL_FACE);
    glLineWidth(1);
    if (mode == 1) glEnable(GL_LINE_SMOOTH);

    int size = axis == 0 ? W : H;      // size of the tested axis
    int other = axis == 0 ? H : W;     // pixels along the other axis; case i uses pixel 3i+1 (lines spill +-1 px)
    int per = other / 3;                // cases per batch
    unsigned char *img = malloc((size_t)W * H * 4);
    int *res = malloc(sizeof(int) * n);
    float *v = malloc(sizeof(float) * 8 * per);
    float z[4] = {0, 0, 0, 0};
    for (int base = 0; base < n; base += per) {
        int cnt = n - base < per ? n - base : per;
        int nv = 0;
        for (int i = 0; i < cnt; i++) {
            float c = in[base + i];
            float o = (float)(((3 * i + 1.5) / other) * 2.0 - 1.0); // centre of pixel 3i+1 of the other axis
            float a[2], b[2];
            if (mode == 0) {
                if (axis == 0) { a[0] = c; a[1] = o; } else { a[0] = o; a[1] = c; }
                v[nv++] = a[0]; v[nv++] = a[1];
            } else {
                // a segment along the other axis through the tested coordinate, +-0.9 pixel around the pixel centre
                float oa = (float)((((3 * i + 1.5) - 0.9) / other) * 2.0 - 1.0), ob = (float)((((3 * i + 1.5) + 0.9) / other) * 2.0 - 1.0);
                if (axis == 0) { a[0] = c; a[1] = oa; b[0] = c; b[1] = ob; } else { a[0] = oa; a[1] = c; b[0] = ob; b[1] = c; }
                v[nv++] = a[0]; v[nv++] = a[1]; v[nv++] = b[0]; v[nv++] = b[1];
            }
        }
        glBufferData(GL_ARRAY_BUFFER, sizeof(float) * nv, v, GL_STREAM_DRAW);
        glClearBufferfv(GL_COLOR, 0, z);
        glDrawArrays(mode == 0 ? GL_POINTS : GL_LINES, 0, nv / 2);
        glFinish();
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, img);
        for (int i = 0; i < cnt; i++) {
            int found = -1;
            for (int t = 0; t < size; t++) {
                int x = axis == 0 ? t : 3 * i + 1, y = axis == 0 ? 3 * i + 1 : t;
                if (img[((size_t)y * W + x) * 4 + 3]) found = found == -1 ? t : -2;
            }
            res[base + i] = found;
        }
    }
    FILE *fo = fopen(argv[6], "wb");
    fwrite(res, 4, n, fo);
    fclose(fo);
    return 0;
}
