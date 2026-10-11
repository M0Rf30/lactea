// Probe: draw GL_LINES (GL_LINE_SMOOTH on, width 1, like projectM's motion vectors) and report the lit pixels.
// usage: lines W H cases.txt out.bin
//   cases.txt: one line per case: x0 y0 x1 y1 (window pixels, y down, double); each case is drawn into a cleared
//   W x H RGBA8 target (white line, blending off); out.bin = for each case W*H bytes (1 = lit).
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
    if (argc < 5) die("usage");
    int W = atoi(argv[1]), H = atoi(argv[2]);
    int smooth = argc > 5 ? atoi(argv[5]) : 1;
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

    // cases
    FILE *f = fopen(argv[3], "r");
    if (!f) die("cases open");
    int cap = 1024, n = 0;
    double (*cs)[4] = malloc(sizeof(double[4]) * cap);
    while (fscanf(f, "%lf %lf %lf %lf", &cs[n][0], &cs[n][1], &cs[n][2], &cs[n][3]) == 4) {
        if (++n == cap) { cap *= 2; cs = realloc(cs, sizeof(double[4]) * cap); }
    }
    fclose(f);

    GLuint vao, vbo;
    glGenVertexArrays(1, &vao); glBindVertexArray(vao);
    glGenBuffers(1, &vbo); glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(float) * 4, NULL, GL_STREAM_DRAW);
    glEnableVertexAttribArray(0); glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 8, (void *)0);
    // like projectM's MV shader: positions arrive in 0..1 (y down), then pos*2-1, y flip, ortho flip (net: y up = 1 - 2y')
    const char *vs = smooth >= 3 ? "#version 330 core\nlayout(location=0) in vec2 pos;\nvoid main(){ gl_Position = vec4(pos, 0.0, 1.0); gl_PointSize = 1.0; }\n" :
                     "#version 330 core\nlayout(location=0) in vec2 pos;\n"
                     "void main(){ vec2 p = pos * 2.0 - 1.0; p.y = -p.y; gl_Position = vec4(p.x, -p.y, 0.0, 1.0); }\n";
    const char *fs = "#version 330 core\nlayout(location=0) out vec4 o;\nvoid main(){ o = vec4(1.0); }\n";
    GLuint p = glCreateProgram();
    glAttachShader(p, sh(GL_VERTEX_SHADER, vs)); glAttachShader(p, sh(GL_FRAGMENT_SHADER, fs));
    glLinkProgram(p); glUseProgram(p);
    GLuint rt, fbo;
    glGenTextures(1, &rt); glBindTexture(GL_TEXTURE_2D, rt);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glGenFramebuffers(1, &fbo); glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) die("fbo incomplete");
    glViewport(0, 0, W, H);
    glDisable(GL_BLEND); glDisable(GL_DEPTH_TEST); glDisable(GL_CULL_FACE);
    glLineWidth(1);
    glEnable(GL_PROGRAM_POINT_SIZE);
    if (smooth == 1 || smooth == 4) glEnable(GL_LINE_SMOOTH);
    unsigned char *img = malloc((size_t)W * H * 4);
    unsigned char *out = malloc((size_t)W * H * 4);
    FILE *fo = fopen(argv[4], "wb");
    float z[4] = {0, 0, 0, 0};
    for (int i = 0; i < n; i++) {
        float v[4] = {(float)(cs[i][0] / W), (float)(1.0 - cs[i][1] / H), (float)(cs[i][2] / W), (float)(1.0 - cs[i][3] / H)};
        if (smooth >= 3) { v[0] = (float)cs[i][0]; v[1] = (float)cs[i][1]; v[2] = (float)cs[i][2]; v[3] = (float)cs[i][3]; }
        glBufferData(GL_ARRAY_BUFFER, sizeof v, v, GL_STREAM_DRAW);
        glClearBufferfv(GL_COLOR, 0, z);
        if (smooth == 2 || smooth == 3) glDrawArrays(GL_POINTS, 0, 1); else glDrawArrays(GL_LINES, 0, 2);
        glFinish();
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, img);
        // GL row 0 = bottom; our window y is down: flip while writing; sparse: n, then n x (u16 x, u16 y)
        unsigned cnt = 0;
        unsigned short *lst = (unsigned short *)out;
        for (int y = 0; y < H; y++)
            for (int x = 0; x < W; x++)
                if (img[((size_t)(H - 1 - y) * W + x) * 4 + 3]) { lst[2 * cnt] = (unsigned short)x; lst[2 * cnt + 1] = (unsigned short)y; cnt++; }
        fwrite(&cnt, 4, 1, fo);
        fwrite(lst, 4, cnt, fo);
    }
    fclose(fo);
    return 0;
}
