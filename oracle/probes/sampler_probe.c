// Probe: bilinear lookups of an RG16F texture in a *vertex shader* (what projectM's motion vector shader does).
// usage: samp W H tex.f32 queries.f32 out.f32 [mode]
//   tex.f32     W*H*2 floats, memory row 0 first (= GL texture row 0), values must be f16-exact
//   queries.f32 N*2 floats: the (x, y) passed to texture(tex, vec2(x, 1.0 - y)) exactly like projectM
//   out.f32     N*2 floats, the texture() result
//   mode 0 = texture(tex, vec2(q.x, 1.0 - q.y)); mode 1 = texture(tex, q) (raw coordinates)
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
    if (argc < 6) die("usage");
    int W = atoi(argv[1]), H = atoi(argv[2]);
    int mode = argc > 6 ? atoi(argv[6]) : 0;
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
    fprintf(stderr, "renderer: %s\n", glGetString(GL_RENDERER));

    float *tex = malloc(sizeof(float) * 2 * W * H);
    FILE *f = fopen(argv[3], "rb");
    if (!f || fread(tex, sizeof(float) * 2, (size_t)W * H, f) != (size_t)W * H) die("tex read");
    fclose(f);
    f = fopen(argv[4], "rb");
    if (!f) die("queries open");
    fseek(f, 0, SEEK_END);
    long fsz = ftell(f);
    fseek(f, 0, SEEK_SET);
    int N = (int)(fsz / (2 * sizeof(float)));
    if (N > W * H) die("too many queries (max W*H)");
    float *q = malloc(sizeof(float) * 2 * N);
    if (fread(q, sizeof(float) * 2, N, f) != (size_t)N) die("queries read");
    fclose(f);

    GLuint vao, vbo;
    glGenVertexArrays(1, &vao); glBindVertexArray(vao);
    glGenBuffers(1, &vbo); glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(float) * 2 * N, q, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0); glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 8, (void *)0);

    char vs[2048];
    snprintf(vs, sizeof vs,
             "#version 330 core\nlayout(location=0) in vec2 pos; uniform sampler2D tex; uniform vec2 inv;\n"
             "flat out vec2 res;\n"
             "void main(){\n"
             "  vec2 oldUV = %s;\n"
             "  res = oldUV;\n"
             "  float px = (float(gl_VertexID %% %d) + 0.5) * inv.x * 2.0 - 1.0;\n"
             "  float py = (float(gl_VertexID / %d) + 0.5) * inv.y * 2.0 - 1.0;\n"
             "  gl_Position = vec4(px, py, 0.0, 1.0); gl_PointSize = 1.0;\n"
             "}\n",
             mode == 0 ? "texture(tex, vec2(pos.x, 1.0 - pos.y)).xy" : "texture(tex, pos).xy", W, W);
    const char *fs = "#version 330 core\nflat in vec2 res; layout(location=0) out vec2 o0;\nvoid main(){ o0 = res; }\n";
    GLuint p = glCreateProgram();
    glAttachShader(p, sh(GL_VERTEX_SHADER, vs)); glAttachShader(p, sh(GL_FRAGMENT_SHADER, fs));
    glLinkProgram(p); glUseProgram(p);
    glUniform1i(glGetUniformLocation(p, "tex"), 0);
    glUniform2f(glGetUniformLocation(p, "inv"), 1.0f / W, 1.0f / H);

    GLuint t, rt, fbo, smp;
    glGenTextures(1, &t); glActiveTexture(GL_TEXTURE0); glBindTexture(GL_TEXTURE_2D, t);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RG16F, W, H, 0, GL_RG, GL_FLOAT, tex);
    glGenSamplers(1, &smp);
    glSamplerParameteri(smp, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glSamplerParameteri(smp, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glSamplerParameteri(smp, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glSamplerParameteri(smp, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glBindSampler(0, smp);
    glGenTextures(1, &rt); glBindTexture(GL_TEXTURE_2D, rt);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RG32F, W, H, 0, GL_RG, GL_FLOAT, NULL);
    glGenFramebuffers(1, &fbo); glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) die("fbo incomplete");
    glViewport(0, 0, W, H);
    glDisable(GL_BLEND); glDisable(GL_DEPTH_TEST); glDisable(GL_CULL_FACE);
    glEnable(GL_PROGRAM_POINT_SIZE);
    float z[4] = {0, 0, 0, 0};
    glClearBufferfv(GL_COLOR, 0, z);
    // the texture unit 0 must be the RG16F one while drawing
    glActiveTexture(GL_TEXTURE0); glBindTexture(GL_TEXTURE_2D, t);
    glDrawArrays(GL_POINTS, 0, N);
    glFinish();
    float *r = malloc(sizeof(float) * 2 * W * H);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glBindTexture(GL_TEXTURE_2D, rt); glGetTexImage(GL_TEXTURE_2D, 0, GL_RG, GL_FLOAT, r);
    f = fopen(argv[5], "wb"); fwrite(r, sizeof(float) * 2, N, f); fclose(f);
    return 0;
}
