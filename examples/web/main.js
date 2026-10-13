import init, { Lactea } from "./pkg/lactea_web_demo.js";

const $ = (id) => document.getElementById(id);
const canvas = $("canvas");
const AUTO_SECONDS = 20;
const BLEND_SECONDS = 2.7;
const PRESET_RE = /\.milk2?$/i;
const TEXTURE_RE = /\.(png|jpe?g|tga|bmp)$/i;

function fail(text) {
  $("msg").textContent = text;
  $("msg").classList.add("show");
  $("ui").style.display = "none";
}

// ---- audio: an AudioWorklet copies interleaved stereo blocks to the main thread -------------
const TAP_SRC = `
class Tap extends AudioWorkletProcessor {
  constructor() { super(); this.buf = new Float32Array(2048); this.n = 0; }
  process(inputs) {
    const ch = inputs[0];
    if (ch.length) {
      const l = ch[0], r = ch[1] || ch[0];
      for (let i = 0; i < l.length; i++) {
        this.buf[this.n++] = l[i];
        this.buf[this.n++] = r[i];
        if (this.n === this.buf.length) { this.port.postMessage(this.buf.slice()); this.n = 0; }
      }
    }
    return true;
  }
}
registerProcessor("lactea-tap", Tap);`;

const audioCtx = new AudioContext();
const pcm = [];
let tap = null;
let micStream = null;
let mediaSource = null;

async function ensureTap() {
  if (tap) return tap;
  const url = URL.createObjectURL(new Blob([TAP_SRC], { type: "text/javascript" }));
  await audioCtx.audioWorklet.addModule(url);
  tap = new AudioWorkletNode(audioCtx, "lactea-tap", { channelCount: 2, channelCountMode: "explicit" });
  tap.port.onmessage = (e) => {
    pcm.push(e.data);
    // the frame loop drains this; while the tab is hidden keep only the last ~0.5 s
    if (pcm.length > 16) pcm.splice(0, pcm.length - 16);
  };
  // a muted sink keeps the worklet pulled by the graph
  const mute = audioCtx.createGain();
  mute.gain.value = 0;
  tap.connect(mute).connect(audioCtx.destination);
  return tap;
}

async function playFile(file) {
  stopMic();
  const node = await ensureTap();
  const player = $("player");
  if (!mediaSource) {
    mediaSource = audioCtx.createMediaElementSource(player);
    mediaSource.connect(audioCtx.destination);
    mediaSource.connect(node);
  }
  if (player.src) URL.revokeObjectURL(player.src);
  player.src = URL.createObjectURL(file);
  await audioCtx.resume();
  await player.play();
}

function stopMic() {
  if (!micStream) return;
  micStream.getTracks().forEach((t) => t.stop());
  micStream = null;
  $("mic").textContent = "🎤 Mic";
}

async function toggleMic() {
  if (micStream) return stopMic();
  $("player").pause();
  const node = await ensureTap();
  micStream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
  });
  audioCtx.createMediaStreamSource(micStream).connect(node);
  await audioCtx.resume();
  $("mic").textContent = "🎤 Stop";
}

// ---- presets ---------------------------------------------------------------------------------
/** @type {{name: string, bytes: () => Promise<Uint8Array>}[]} */
let playlist = [];
let index = -1;
let lastSwitch = 0;
let lactea = null;

async function show(i, blend = BLEND_SECONDS) {
  if (!playlist.length) return;
  // a preset that fails to load is skipped; give up after one pass over the playlist
  for (let tries = 0; tries < playlist.length; tries++) {
    index = ((i % playlist.length) + playlist.length) % playlist.length;
    const p = playlist[index];
    try {
      lactea.load_preset(await p.bytes(), p.name, blend);
      $("name").textContent = `${index + 1}/${playlist.length} · ${p.name}`;
      lastSwitch = performance.now();
      return;
    } catch (e) {
      console.warn(`skipping ${p.name}: ${e.message ?? e}`);
      i = index + 1;
    }
  }
  $("name").textContent = "no preset in the playlist could be loaded";
}

const next = () => show(index + 1);
const prev = () => show(index - 1);
const random = () => show(Math.floor(Math.random() * playlist.length));

async function addFiles(files) {
  const presets = [];
  let audio = null;
  for (const f of files) {
    if (PRESET_RE.test(f.name)) {
      presets.push({ name: f.name.replace(PRESET_RE, ""), bytes: async () => new Uint8Array(await f.arrayBuffer()) });
    } else if (TEXTURE_RE.test(f.name)) {
      try {
        lactea.add_texture(f.name, new Uint8Array(await f.arrayBuffer()));
      } catch (e) {
        console.warn(`texture ${f.name}: ${e.message ?? e}`);
      }
    } else if (f.type.startsWith("audio/")) {
      audio = f;
    }
  }
  if (presets.length) {
    presets.sort((a, b) => a.name.localeCompare(b.name));
    playlist = presets;
    await show(0);
  }
  if (audio) await playFile(audio).catch((e) => console.warn(`audio: ${e.message ?? e}`));
}

// Two small presets written for this page: what plays when no preset pack is deployed.
const BUILTIN = [
  { name: "tunnel", milk: `[preset00]
fDecay=0.97
zoom=1.03
rot=0.02
fWaveAlpha=0.9
nWaveMode=2
wave_r=0.4
wave_g=0.8
wave_b=1
ob_size=0.015
ob_r=1
ob_a=0.6
per_frame_1=zoom = 1.0 + 0.03*sin(time*0.7) + 0.02*bass;
per_frame_2=wave_r = 0.5 + 0.5*sin(time); wave_b = 0.5 + 0.5*cos(time*1.3);
` },
  { name: "spiral", milk: `[preset00]
fDecay=0.95
zoom=1.0
rot=0.06
warp=1.2
fWaveAlpha=1
nWaveMode=0
wave_r=1
wave_g=0.5
wave_b=0.2
per_frame_1=rot = 0.04*sin(time*0.5) + 0.03*treb;
per_frame_2=wave_g = 0.5 + 0.5*sin(time*2); zoom = 1.01 + 0.02*bass;
` },
];

/** Presets deployed next to the page (`PRESETS=dir build.sh`), else the built-in ones. */
async function loadBundled() {
  const builtin = () => {
    const enc = new TextEncoder();
    playlist = BUILTIN.map((p) => ({ name: p.name, bytes: async () => enc.encode(p.milk) }));
    return show(0, 0);
  };
  let index;
  try {
    const r = await fetch("presets/index.json");
    if (!r.ok) return builtin();
    index = await r.json();
  } catch {
    return builtin();
  }
  await Promise.all(
    (index.textures ?? []).map(async (n) => {
      try {
        const r = await fetch(`presets/${encodeURIComponent(n)}`);
        lactea.add_texture(n, new Uint8Array(await r.arrayBuffer()));
      } catch (e) {
        console.warn(`texture ${n}: ${e.message ?? e}`);
      }
    }),
  );
  playlist = (index.presets ?? []).map((n) => ({
    name: n.replace(PRESET_RE, ""),
    bytes: async () => {
      const r = await fetch(`presets/${encodeURIComponent(n)}`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      return new Uint8Array(await r.arrayBuffer());
    },
  }));
  if (playlist.length) await random();
  else await builtin();
}

// ---- frame loop ------------------------------------------------------------------------------
function fitCanvas() {
  const dpr = Math.min(window.devicePixelRatio || 1, 1.5);
  const w = Math.max(1, Math.floor(innerWidth * dpr));
  const h = Math.max(1, Math.floor(innerHeight * dpr));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  return [w, h];
}

let synthEnd = 0;
/** Interleaved stereo synthetic beat from the last call up to `t` seconds (at most 0.1 s). */
function synthBeat(t) {
  const rate = audioCtx.sampleRate;
  const start = Math.max(synthEnd, t - 0.1);
  const n = Math.max(0, Math.round((t - start) * rate));
  synthEnd = start + n / rate;
  const out = new Float32Array(2 * n);
  for (let i = 0; i < n; i++) {
    const s = start + i / rate;
    const beat = (s * 2) % 1;
    const kick = Math.sin(2 * Math.PI * 55 * s) * Math.exp(-beat * 8);
    const saw = (((s * 220) % 1) * 2 - 1) * 0.15;
    out[2 * i] = out[2 * i + 1] = 0.8 * kick + saw;
  }
  return out;
}

let frame = 0;
let t0 = 0;
function tick(now) {
  const [w, h] = fitCanvas();
  try {
    lactea.resize(w, h);
  } catch (e) {
    console.warn(`resize: ${e.message ?? e}`);
  }
  for (const block of pcm) lactea.push_pcm(block, 2);
  pcm.length = 0;
  // no audio source yet: a synthetic 120 bpm kick + saw keeps the presets moving
  if (!micStream && $("player").paused) lactea.push_pcm(synthBeat(now / 1000), 2);
  if ($("auto").checked && playlist.length > 1 && now - lastSwitch > AUTO_SECONDS * 1000) {
    lastSwitch = now;
    next();
  }
  try {
    lactea.render((now - t0) / 1000, frame++);
  } catch (e) {
    return fail(`Render error: ${e.message ?? e}`);
  }
  requestAnimationFrame(tick);
}

// ---- input -----------------------------------------------------------------------------------
function wireUi() {
  $("next").onclick = next;
  $("prev").onclick = prev;
  $("rand").onclick = random;
  $("presetFiles").onchange = (e) => addFiles(e.target.files);
  $("presetDir").onchange = (e) => addFiles(e.target.files);
  $("audioFile").onchange = (e) =>
    e.target.files[0] && playFile(e.target.files[0]).catch((err) => alert(`Audio: ${err.message ?? err}`));
  $("mic").onclick = () => toggleMic().catch((e) => alert(`Microphone: ${e.message ?? e}`));
  $("fs").onclick = toggleFullscreen;

  addEventListener("keydown", (e) => {
    if (e.target instanceof HTMLInputElement && e.target.type !== "checkbox") return;
    switch (e.key.toLowerCase()) {
      case "n": case " ": next(); break;
      case "p": prev(); break;
      case "r": random(); break;
      case "f": toggleFullscreen(); break;
      case "h": document.body.classList.toggle("idle"); return;
      default: return;
    }
    e.preventDefault();
  });

  let hideTimer = 0;
  const poke = () => {
    document.body.classList.remove("idle");
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => document.body.classList.add("idle"), 3000);
  };
  addEventListener("pointermove", poke);
  poke();

  canvas.addEventListener("pointermove", (e) => lactea.set_mouse(e.clientX / innerWidth, e.clientY / innerHeight, e.buttons === 1));
  canvas.addEventListener("pointerleave", () => lactea.set_mouse(undefined, undefined, false));

  addEventListener("dragover", (e) => { e.preventDefault(); document.body.classList.add("dragging"); });
  addEventListener("dragleave", (e) => { if (!e.relatedTarget) document.body.classList.remove("dragging"); });
  addEventListener("drop", (e) => {
    e.preventDefault();
    document.body.classList.remove("dragging");
    addFiles(e.dataTransfer.files);
  });
}

function toggleFullscreen() {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.documentElement.requestFullscreen();
}

// ---- start -----------------------------------------------------------------------------------
async function main() {
  if (!navigator.gpu) {
    return fail("This demo needs WebGPU (Chrome/Edge 113+, Safari 26+, Firefox 141+ on Windows).");
  }
  await init();
  fitCanvas();
  try {
    lactea = await Lactea.create(canvas, audioCtx.sampleRate, (Math.random() * 2 ** 32) >>> 0);
  } catch (e) {
    return fail(`Could not start: ${e.message ?? e}`);
  }
  wireUi();
  await loadBundled();
  requestAnimationFrame((now) => {
    t0 = now;
    tick(now);
  });
}

main();
