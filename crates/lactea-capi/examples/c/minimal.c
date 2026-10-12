/* Minimal lactea C API host: renders 10 frames of a preset into a CPU RGBA8 buffer and checks
 * the last frame is not black. It also registers a switch-requested callback (short display time)
 * that loads the preset again from inside the callback, and checks the callback ran through
 * lactea_render_to_buffer.
 *
 *   cc minimal.c -I../../include -L<dir with liblactea_capi> -llactea_capi -lm -o minimal
 *   ./minimal [preset.milk [out.ppm]]
 *
 * Exit status: 0 = rendered non-black output and the callback ran, 1 = failure. */
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lactea.h"

/* Used when no preset file is given. */
static const char *BUILTIN_PRESET =
    "MILKDROP_PRESET_VERSION=100\n"
    "fDecay=0.95\nfWaveAlpha=0.9\nnWaveMode=0\nzoom=1.02\nrot=0.01\nwarp=0.5\nfGammaAdj=1.5\n"
    "ob_a=0.5\nob_r=1\nob_size=0.02\nbDarkenCenter=1\n"
    "per_frame_1=zoom = 1.0 + 0.02*sin(time*3) + 0.01*bass;\n"
    "per_frame_2=q1 = q1 + 0.1; wave_r = 0.5+0.5*sin(time);\n";

static int die(const char *what) {
    fprintf(stderr, "%s: %s\n", what, lactea_last_error());
    return 1;
}

/* State of the switch-requested callback. */
typedef struct {
    LacteaEngine *engine;
    const char *preset;
    size_t preset_len;
    unsigned requests;
    LacteaStatus reload; /* status of the load made inside the callback */
} Host;

/* "Pick the next preset": the callback may call back into the handle that fired it. */
static void on_request(bool is_hard_cut, void *user) {
    Host *host = user;
    (void)is_hard_cut;
    host->requests++;
    host->reload = lactea_load_preset_ex(host->engine, (const uint8_t *)host->preset,
                                         host->preset_len, "minimal (reloaded)", 0.0);
}

int main(int argc, char **argv) {
    LacteaConfig cfg;
    lactea_config_default(&cfg);
    cfg.width = 320;
    cfg.height = 180;

    LacteaEngine *engine = lactea_create(&cfg);
    if (!engine) return die("lactea_create");
    printf("lactea %s, %ux%u\n", lactea_version(), lactea_width(engine), lactea_height(engine));

    const char *preset = BUILTIN_PRESET;
    size_t preset_len = strlen(BUILTIN_PRESET);
    char *file_data = NULL;
    if (argc > 1) {
        FILE *f = fopen(argv[1], "rb");
        if (!f) { perror(argv[1]); return 1; }
        fseek(f, 0, SEEK_END);
        long n = ftell(f);
        fseek(f, 0, SEEK_SET);
        file_data = malloc((size_t)n);
        if (!file_data || fread(file_data, 1, (size_t)n, f) != (size_t)n) { perror("read"); return 1; }
        fclose(f);
        preset = file_data;
        preset_len = (size_t)n;
    }
    LacteaSwitchSettings settings;
    if (lactea_get_switch_settings(engine, &settings) != LACTEA_STATUS_OK)
        return die("lactea_get_switch_settings");
    settings.preset_duration = 0.05; /* a request after about 3 frames */
    settings.easter_egg = 0.0f;      /* no random part: the request time is exact */
    if (lactea_set_switch_settings(engine, &settings) != LACTEA_STATUS_OK)
        return die("lactea_set_switch_settings");

    Host host = {engine, preset, preset_len, 0, LACTEA_STATUS_OK};
    if (lactea_set_switch_requested_callback(engine, on_request, &host) != LACTEA_STATUS_OK)
        return die("lactea_set_switch_requested_callback");

    if (lactea_load_preset(engine, (const uint8_t *)preset, preset_len, "minimal") != LACTEA_STATUS_OK)
        return die("lactea_load_preset");

    size_t bytes = (size_t)lactea_width(engine) * lactea_height(engine) * 4;
    uint8_t *pixels = malloc(bytes);
    if (!pixels) return 1;

    const int fps = (int)cfg.fps;
    const int hop = (int)cfg.sample_rate / fps; /* PCM frames per video frame */
    float *pcm = malloc((size_t)hop * 2 * sizeof(float));
    unsigned long long t = 0;
    for (int frame = 0; frame < 10; frame++) {
        for (int i = 0; i < hop; i++, t++) {
            float s = 0.5f * sinf(2.0f * 3.14159265f * 220.0f * (float)t / (float)cfg.sample_rate);
            pcm[2 * i] = s;
            pcm[2 * i + 1] = s;
        }
        if (lactea_push_pcm_f32(engine, pcm, (size_t)hop, 2) != LACTEA_STATUS_OK) return die("push_pcm");
        if (lactea_render_to_buffer(engine, (double)frame / fps, (uint32_t)frame, pixels, bytes) !=
            LACTEA_STATUS_OK)
            return die("lactea_render_to_buffer");
    }

    unsigned long long sum = 0;
    for (size_t i = 0; i < bytes; i += 4) sum += pixels[i] + pixels[i + 1] + pixels[i + 2];
    printf("sum of RGB over the last frame: %llu\n", sum);
    printf("switch requests seen by the callback: %u\n", host.requests);

    if (argc > 2) {
        FILE *f = fopen(argv[2], "wb");
        if (f) {
            fprintf(f, "P6\n%u %u\n255\n", lactea_width(engine), lactea_height(engine));
            for (size_t i = 0; i < bytes; i += 4) fwrite(pixels + i, 1, 3, f);
            fclose(f);
        }
    }

    free(pcm);
    free(pixels);
    free(file_data);
    lactea_destroy(engine);
    if (sum == 0) {
        fprintf(stderr, "output is black\n");
        return 1;
    }
    if (host.requests == 0) {
        fprintf(stderr, "the switch-requested callback never ran under lactea_render_to_buffer\n");
        return 1;
    }
    if (host.reload != LACTEA_STATUS_OK) {
        fprintf(stderr, "lactea_load_preset_ex inside the callback failed (%d)\n", (int)host.reload);
        return 1;
    }
    return 0;
}
